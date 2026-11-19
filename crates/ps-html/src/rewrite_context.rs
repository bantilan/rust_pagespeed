//! Rewrite orchestration (`RewriteContext`, sync completion).
//!
//! Runs one rewrite end to end: partition-key computation, metadata
//! lookup with validation and revalidation, partitioning, input
//! fetching, rewriting, partition write-back, and slot rendering.
//! The C++ lock/dedup/async machinery collapses here — fetchers and
//! caches complete inline, so each context runs start-to-finish in
//! one call; concurrent same-key sharing falls out of the metadata
//! cache instead of the repeated-rewrite lists. Serving, nesting,
//! fetch rewrites, and successors land next.

use std::cell::RefCell;
use std::rc::Rc;

use ps_cache::cache::KeyState;
use ps_cache::ttl::FetchStatus;
use ps_http::content_type::ContentTypeKind;
use ps_http::fetch::AsyncFetch;
use ps_http::google_url::GoogleUrl;

use super::cacheable_resource::{InputInfo, InputInfoType, MockLoadCallback, NotCacheablePolicy};
use super::metadata::{
    decode_partitions, encode_partitions, is_input_valid, CachedResult, InputValidity,
    OutputPartitions,
};
use super::output_resource::{OutputResource, OutputResourceKind};
use super::parser::Dom;
use super::resource::{InputResource, Resource, ResourceHandle};
use super::resource_slot::{AssociationSlot, CssResourceSlot, FetchResourceSlot, HtmlResourceSlot};
use super::server_context::{InlineAuthorizationPolicy, InputRole, IntendedFor, ResourceDriver};
use super::url_segment_encoder::{DefaultEncoder, UrlSegmentEncoder};

/// Shared default encoder.
static DEFAULT_ENCODER: DefaultEncoder = DefaultEncoder;

/// Rewrite outcome (`RewriteResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteResult {
    /// Rewrote.
    Ok,
    /// Cannot rewrite (remembered).
    Failed,
    /// System busy (retried, never remembered).
    TooBusy,
}

/// Per-filter rewrite behavior (the `SingleRewriteContext`
/// subclass surface: identity plus the single-input rewrite).
pub trait SingleRewriteBehavior {
    /// Filter id prefix.
    fn id(&self) -> &'static str;

    /// Output kind.
    fn kind(&self) -> OutputResourceKind {
        OutputResourceKind::Rewritten
    }

    /// URL segment codec.
    fn encoder(&self) -> &'static dyn UrlSegmentEncoder {
        &DEFAULT_ENCODER
    }

    /// Encodes an output name (`encoder()->Encode` with context).
    fn encode_output_name(&self, leaf: &[u8]) -> Vec<u8> {
        self.encoder().encode(&[leaf.to_vec()])
    }

    /// Decodes an output name (`encoder()->Decode` with context).
    fn decode_output_name(&self, name: &[u8]) -> Option<Vec<Vec<u8>>> {
        self.encoder().decode(name)
    }

    /// Extra partition-key suffix (dimensions, UA keys).
    fn key_suffix_extra(&self) -> String {
        String::new()
    }

    /// Custom partition rendering (`Render` override): true skips
    /// the default slot rendering for the partition.
    #[allow(clippy::too_many_arguments)]
    fn render_partition(
        &self,
        _driver: &Rc<ResourceDriver>,
        _dom: &mut Dom,
        _base: &GoogleUrl,
        _options: &ps_config::options::RewriteOptions,
        _partition: &CachedResult,
        _slots: &[ContextSlot],
        _outputs: &[Option<OutputResource>],
        _slot_indices: &[usize],
    ) -> bool {
        false
    }

    /// Whether rendering may proceed (`PolicyPermitsRendering`).
    fn policy_permits_rendering(
        &self,
        _driver: &Rc<ResourceDriver>,
        _outputs: &[Option<OutputResource>],
    ) -> bool {
        true
    }

    /// Runs a context-level render (`Render`: statistics, debug
    /// comments, canonical-URL patching).
    fn note_render(
        &self,
        _driver: &Rc<ResourceDriver>,
        _dom: &mut Dom,
        _partitions: &OutputPartitions,
        _slots: &[ContextSlot],
    ) {
    }

    /// Whether serving falls back to the original on failure
    /// (`OptimizationOnly`).
    fn optimization_only(&self) -> bool {
        true
    }

    /// Whether a wrong-hash serve fails outright
    /// (`FailOnHashMismatch`).
    fn fail_on_hash_mismatch(&self) -> bool {
        false
    }

    /// Filter extras after fallback shortening (filter overrides of
    /// `FixFetchFallbackHeaders`, e.g. canonical links).
    fn extra_fetch_fallback_headers(
        &self,
        _driver: &Rc<ResourceDriver>,
        _inputs: &[InputResource],
        _headers: &mut ps_http::response::ResponseHeaders,
    ) {
    }

    /// Rewrite one loaded input into its output (`RewriteSingle`).
    fn rewrite_single(
        &self,
        driver: &Rc<ResourceDriver>,
        input: &InputResource,
        output: &mut OutputResource,
        partition: &mut CachedResult,
    ) -> RewriteResult;

    /// Sends fallback bytes past the content-type gate
    /// (`SendFallbackResponse`; filters re-resolve URLs here).
    fn send_fallback_response(
        &self,
        _driver: &Rc<ResourceDriver>,
        _inputs: &[InputResource],
        _original_url: &str,
        contents: &[u8],
        fetch: &mut dyn AsyncFetch,
    ) -> bool {
        let allowed = fetch
            .response_headers()
            .determine_content_type()
            .is_some_and(|content_type| {
                use ps_http::content_type::ContentTypeKind;
                content_type.is_js_like()
                    || content_type.is_css()
                    || content_type.is_image()
                    || content_type.kind() == ContentTypeKind::Pdf
            });
        if !allowed {
            return false;
        }
        fetch.headers_complete();
        fetch.handle_write(contents)
    }
}

/// Metadata lookup verdict.
enum CacheDecision {
    /// Fresh entry; render from cache.
    Hit,
    /// Stale but revalidatable; carries (partition, input) positions.
    Revalidate(Vec<(usize, usize)>),
    /// Absent, corrupt, or unsalvageable.
    Miss,
}

/// A slot in any rewrite mode (`ResourceSlotPtr`: HTML rewrites
/// render into the DOM, srcset candidates defer to commit, fetch
/// rewrites only borrow resources).
#[derive(Debug, Clone)]
pub enum ContextSlot {
    /// DOM attribute slot.
    Html(Rc<RefCell<HtmlResourceSlot>>),
    /// Srcset candidate slot.
    SrcSet(Rc<RefCell<super::srcset::SrcSetSlot>>),
    /// Fetch reconstruction slot.
    Fetch(Rc<RefCell<FetchResourceSlot>>),
    /// CSS `url()` slot.
    Css(Rc<RefCell<CssResourceSlot>>),
    /// Fallback association slot.
    Assoc(Rc<RefCell<AssociationSlot>>),
}

impl ContextSlot {
    /// Whether later filters must leave the slot alone.
    fn disable_further_processing(&self) -> bool {
        match self {
            ContextSlot::Html(slot) => slot.borrow().base().disable_further_processing(),
            ContextSlot::SrcSet(slot) => slot.borrow().base().disable_further_processing(),
            ContextSlot::Fetch(slot) => slot.borrow().base().disable_further_processing(),
            ContextSlot::Css(slot) => slot.borrow().base().disable_further_processing(),
            ContextSlot::Assoc(slot) => slot.borrow().base().disable_further_processing(),
        }
    }

    /// Marks the slot optimized.
    pub(crate) fn set_was_optimized(&self, optimized: bool) {
        match self {
            ContextSlot::Html(slot) => {
                slot.borrow_mut().base_mut().set_was_optimized(optimized);
            }
            ContextSlot::SrcSet(slot) => {
                slot.borrow_mut().base_mut().set_was_optimized(optimized);
            }
            ContextSlot::Fetch(slot) => {
                slot.borrow_mut().base_mut().set_was_optimized(optimized);
            }
            ContextSlot::Css(slot) => {
                slot.borrow_mut().base_mut().set_was_optimized(optimized);
            }
            ContextSlot::Assoc(slot) => {
                slot.borrow_mut().base_mut().set_was_optimized(optimized);
            }
        }
    }

    /// Whether some context optimized the slot.
    pub(crate) fn was_optimized(&self) -> bool {
        match self {
            ContextSlot::Html(slot) => slot.borrow().base().was_optimized(),
            ContextSlot::SrcSet(slot) => slot.borrow().base().was_optimized(),
            ContextSlot::Fetch(slot) => slot.borrow().base().was_optimized(),
            ContextSlot::Css(slot) => slot.borrow().base().was_optimized(),
            ContextSlot::Assoc(slot) => slot.borrow().base().was_optimized(),
        }
    }

    /// Halts later filters on the slot.
    fn set_disable_further_processing(&self, disable: bool) {
        match self {
            ContextSlot::Html(slot) => {
                slot.borrow_mut()
                    .base_mut()
                    .set_disable_further_processing(disable);
            }
            ContextSlot::SrcSet(slot) => {
                slot.borrow_mut()
                    .base_mut()
                    .set_disable_further_processing(disable);
            }
            ContextSlot::Fetch(slot) => {
                slot.borrow_mut()
                    .base_mut()
                    .set_disable_further_processing(disable);
            }
            ContextSlot::Css(slot) => {
                slot.borrow_mut()
                    .base_mut()
                    .set_disable_further_processing(disable);
            }
            ContextSlot::Assoc(slot) => {
                slot.borrow_mut()
                    .base_mut()
                    .set_disable_further_processing(disable);
            }
        }
    }

    /// Suppresses slot rendering.
    pub(crate) fn set_disable_rendering(&self) {
        match self {
            ContextSlot::Html(slot) => {
                slot.borrow_mut().base_mut().set_disable_rendering(true);
            }
            ContextSlot::SrcSet(slot) => {
                slot.borrow_mut().base_mut().set_disable_rendering(true);
            }
            ContextSlot::Fetch(slot) => {
                slot.borrow_mut().base_mut().set_disable_rendering(true);
            }
            ContextSlot::Css(slot) => {
                slot.borrow_mut().base_mut().set_disable_rendering(true);
            }
            ContextSlot::Assoc(slot) => {
                slot.borrow_mut().base_mut().set_disable_rendering(true);
            }
        }
    }

    /// Whether rendering deletes the element.
    fn should_delete_element(&self) -> bool {
        match self {
            ContextSlot::Html(slot) => slot.borrow().base().should_delete_element(),
            ContextSlot::SrcSet(slot) => slot.borrow().base().should_delete_element(),
            ContextSlot::Fetch(slot) => slot.borrow().base().should_delete_element(),
            ContextSlot::Css(slot) => slot.borrow().base().should_delete_element(),
            ContextSlot::Assoc(slot) => slot.borrow().base().should_delete_element(),
        }
    }

    /// Rebinds the slot (`SetResource`).
    fn set_resource(&self, resource: ResourceHandle) {
        match self {
            ContextSlot::Html(slot) => slot.borrow_mut().base_mut().set_resource(resource),
            ContextSlot::SrcSet(slot) => slot.borrow_mut().base_mut().set_resource(resource),
            ContextSlot::Fetch(slot) => slot.borrow_mut().base_mut().set_resource(resource),
            ContextSlot::Css(slot) => slot.borrow_mut().base_mut().set_resource(resource),
            ContextSlot::Assoc(slot) => slot.borrow_mut().base_mut().set_resource(resource),
        }
    }

    /// Renders HTML, srcset, and CSS slots; fetch slots render nothing.
    fn render(
        &self,
        dom: &mut Dom,
        base: &GoogleUrl,
        options: &ps_config::options::RewriteOptions,
    ) {
        match self {
            ContextSlot::Html(slot) => slot.borrow_mut().render(dom, base, options),
            ContextSlot::SrcSet(slot) => slot.borrow_mut().render(base, options),
            ContextSlot::Fetch(_) => {}
            ContextSlot::Css(slot) => slot.borrow_mut().render(options),
            ContextSlot::Assoc(slot) => slot.borrow_mut().render(),
        }
    }

    /// Borrows the slot element, if it has one.
    pub fn element(&self) -> Option<crate::element::NodeId> {
        match self {
            ContextSlot::Html(slot) => slot.borrow().element(),
            ContextSlot::SrcSet(slot) => Some(slot.borrow().element()),
            ContextSlot::Fetch(_) | ContextSlot::Css(_) | ContextSlot::Assoc(_) => None,
        }
    }

    /// Patches a URL past the naming scheme (`DirectSetUrl`;
    /// refused when unauthorized; srcset/fetch slots refuse).
    pub fn direct_set_url(&self, dom: &mut Dom, url: &str) -> bool {
        match self {
            ContextSlot::Html(slot) => slot.borrow().direct_set_url(dom, url),
            ContextSlot::SrcSet(_) | ContextSlot::Fetch(_) => false,
            ContextSlot::Css(slot) => slot.borrow().direct_set_url(url),
            ContextSlot::Assoc(slot) => slot.borrow().direct_set_url(url),
        }
    }
}

/// Fetch-rewrite state (`FetchContext`: deadlines, detach, and byte
/// budgets are mute — the sync model serves inline).
struct FetchState {
    /// Requested content hash (empty when the URL names none).
    requested_hash: String,
    /// URL served under (for fallback absolutification, with the
    /// handler-served messages).
    #[allow(dead_code)]
    original_url: String,
    /// Whether the rewrite produced bytes.
    success: bool,
}

/// Mismatch body (`RewriteContext::kHashMismatchMessage`).
const HASH_MISMATCH_MESSAGE: &str = "Hash from URL does not match rewritten hash.";

/// Adds a canonical link unless present or domain-mapped
/// (`AddLinkRelCanonical`).
pub fn add_link_rel_canonical(
    driver: &ResourceDriver,
    input: &InputResource,
    output: &mut ps_http::response::ResponseHeaders,
) {
    if output.has_link_rel_canonical() || input.response_headers().has_link_rel_canonical() {
        return;
    }
    // Complex domain mappings make canonicalization unclear.
    let input_gurl = GoogleUrl::new(input.url());
    if driver.lawyer().will_domain_change(&input_gurl) {
        return;
    }
    if !driver
        .lawyer()
        .find_domains_rewritten_to(&input_gurl)
        .is_empty()
    {
        return;
    }
    output.headers_mut().add(
        ps_http::names::header::LINK,
        &ps_http::response::ResponseHeaders::rel_canonical_header_value(input.url()),
    );
    output.compute_caching();
}

/// One single-input rewrite (`RewriteContext` + `SingleRewriteContext`).
pub struct SingleRewriteContext<B: SingleRewriteBehavior> {
    driver: Rc<ResourceDriver>,
    behavior: B,
    slots: Vec<ContextSlot>,
    inputs: Vec<InputResource>,
    outputs: Vec<Option<OutputResource>>,
    partitions: OutputPartitions,
    partition_key: String,
    render_slots: Vec<bool>,
    rewrite_done: bool,
    ok_to_write_partitions: bool,
    was_too_busy: bool,
    fetch: Option<FetchState>,
    fetch_output: Option<OutputResource>,
}

impl<B: SingleRewriteBehavior> SingleRewriteContext<B> {
    /// Builds a context over a driver.
    pub fn new(driver: Rc<ResourceDriver>, behavior: B) -> Self {
        SingleRewriteContext {
            driver,
            behavior,
            slots: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            partitions: OutputPartitions::default(),
            partition_key: String::new(),
            render_slots: Vec::new(),
            rewrite_done: false,
            ok_to_write_partitions: true,
            was_too_busy: false,
            fetch: None,
            fetch_output: None,
        }
    }

    /// Attaches a slot with its owned input (`AddSlot`; the input
    /// rides alongside because sync ownership splits here — slot
    /// snapshots stay valid since URLs and authorization never
    /// change after creation).
    pub fn add_slot(&mut self, slot: Rc<RefCell<HtmlResourceSlot>>, input: InputResource) {
        self.slots.push(ContextSlot::Html(slot));
        self.inputs.push(input);
        self.render_slots.push(false);
    }

    /// Attaches a srcset candidate slot.
    pub fn add_srcset_slot(
        &mut self,
        slot: Rc<RefCell<super::srcset::SrcSetSlot>>,
        input: InputResource,
    ) {
        self.slots.push(ContextSlot::SrcSet(slot));
        self.inputs.push(input);
        self.render_slots.push(false);
    }

    /// Attaches a CSS `url()` slot.
    pub fn add_css_slot(&mut self, slot: Rc<RefCell<CssResourceSlot>>, input: InputResource) {
        self.slots.push(ContextSlot::Css(slot));
        self.inputs.push(input);
        self.render_slots.push(false);
    }

    /// Attaches a fallback association slot.
    pub fn add_assoc_slot(&mut self, slot: Rc<RefCell<AssociationSlot>>, input: InputResource) {
        self.slots.push(ContextSlot::Assoc(slot));
        self.inputs.push(input);
        self.render_slots.push(false);
    }

    /// Attaches a fetch slot (`AddSlot` with a `FetchResourceSlot`).
    fn add_fetch_slot(&mut self, input: InputResource) {
        let handle: ResourceHandle = Rc::new(input.clone());
        self.slots.push(ContextSlot::Fetch(Rc::new(RefCell::new(
            FetchResourceSlot::new(Some(handle)),
        ))));
        self.inputs.push(input);
        self.render_slots.push(false);
    }

    /// Borrows the partition key (valid after [`start`](Self::start)).
    #[must_use]
    pub fn partition_key(&self) -> &str {
        &self.partition_key
    }

    /// Drains table- and partition-level debug messages (nested
    /// harvest into a parent context; the persisted table is
    /// unaffected since draining happens after the write).
    pub(crate) fn take_debug_messages(&mut self) -> Vec<String> {
        let mut out = std::mem::take(&mut self.partitions.debug_message);
        for partition in &mut self.partitions.partition {
            out.append(&mut partition.debug_message);
        }
        out
    }

    /// Runs the rewrite to completion inline (`Start` + the sync
    /// completion chain), rendering into `dom`.
    pub fn start(&mut self, dom: &mut Dom) {
        // Sick metadata caches drop contexts (`InitiateRewrite`).
        if !self.driver.can_rewrite_resources() {
            self.rewrite_done = true;
            return;
        }
        // Disabled slots bail out immediately.
        if self
            .slots
            .iter()
            .any(ContextSlot::disable_further_processing)
        {
            self.rewrite_done = true;
            self.propagate(dom, false);
            return;
        }
        self.set_partition_key();
        match self.lookup_partitions() {
            CacheDecision::Hit => self.output_cache_hit(dom, false),
            CacheDecision::Revalidate(positions) => self.output_cache_revalidate(dom, positions),
            CacheDecision::Miss => self.output_cache_miss(dom),
        }
    }

    /// Computes the partition key (`SetPartitionKey`).
    fn set_partition_key(&mut self) {
        let signature = self.driver.partition_signature_hash();
        let suffix = self.key_suffix();
        let url_key = if self.slots.len() == 1 {
            // NOTE: data-URL keys hash-split in C++; untested flows
            // keep the raw key until lock hashing lands.
            self.inputs[0].cache_key().to_string()
        } else if self.slots.is_empty() {
            "empty".to_string()
        } else {
            let keys: Vec<Vec<u8>> = self
                .inputs
                .iter()
                .map(|input| input.cache_key().as_bytes().to_vec())
                .collect();
            String::from_utf8_lossy(&self.behavior.encoder().encode(&keys)).into_owned()
        };
        self.partition_key = format!(
            "rname/{}_{}/{url_key}@{suffix}",
            self.behavior.id(),
            signature
        );
    }

    /// Suffix half of the partition key (encoder + UA + kind suffixes,
    /// all empty here).
    fn key_suffix(&self) -> String {
        let encoding = self.behavior.encoder().encode(&[Vec::new()]);
        format!(
            "{}@_{}",
            String::from_utf8_lossy(&encoding),
            self.behavior.key_suffix_extra()
        )
    }

    /// Looks up and validates the partition table.
    fn lookup_partitions(&mut self) -> CacheDecision {
        let lookup = self
            .driver
            .server_context()
            .metadata_cache()
            .get(&self.partition_key);
        if lookup.state != KeyState::Available {
            self.driver
                .server_context()
                .variable("rewrite_cached_output_misses")
                .add(1);
            return CacheDecision::Miss;
        }
        let bytes = lookup.value.unwrap_or_default();
        let Some(tables) = decode_partitions(&bytes) else {
            // Corrupt entries read as misses (never revalidated).
            self.driver
                .server_context()
                .variable("rewrite_cached_output_misses")
                .add(1);
            return CacheDecision::Miss;
        };
        if !self.other_dependencies_valid(&tables) || !self.slot_indexes_valid(&tables) {
            self.driver
                .server_context()
                .variable("rewrite_cached_output_misses")
                .add(1);
            return CacheDecision::Miss;
        }
        let mut revalidate = Vec::new();
        let mut can_revalidate = true;
        let mut all_valid = true;
        for (partition_index, partition) in tables.partition.iter().enumerate() {
            let mut partition_positions = Vec::new();
            let mut partition_can = true;
            if !self.partition_valid(partition, &mut partition_can, &mut partition_positions) {
                all_valid = false;
                can_revalidate = can_revalidate && partition_can;
                revalidate.extend(
                    partition_positions
                        .into_iter()
                        .map(|input_position| (partition_index, input_position)),
                );
            }
        }
        if all_valid {
            self.partitions = tables;
            self.create_outputs_for_hit();
            CacheDecision::Hit
        } else if can_revalidate {
            self.partitions = tables;
            self.create_outputs_for_hit();
            CacheDecision::Revalidate(revalidate)
        } else {
            self.driver
                .server_context()
                .variable("rewrite_cached_output_misses")
                .add(1);
            CacheDecision::Miss
        }
    }

    /// Validates forced-recheck dependencies.
    fn other_dependencies_valid(&self, tables: &OutputPartitions) -> bool {
        tables.other_dependency.iter().all(|input| {
            matches!(
                self.input_validity(input),
                InputValidity::Valid | InputValidity::Stale
            )
        })
    }

    /// Rejects entries naming missing slots.
    fn slot_indexes_valid(&self, tables: &OutputPartitions) -> bool {
        tables.partition.iter().all(|partition| {
            partition.input.iter().all(|input| {
                input
                    .index
                    .is_some_and(|index| index >= 0 && (index as usize) < self.slots.len())
            })
        })
    }

    /// Validates one partition, collecting revalidation positions.
    fn partition_valid(
        &self,
        partition: &CachedResult,
        can_revalidate: &mut bool,
        revalidate: &mut Vec<usize>,
    ) -> bool {
        let mut valid = true;
        for (position, input) in partition.input.iter().enumerate() {
            if !matches!(
                self.input_validity(input),
                InputValidity::Valid | InputValidity::Stale
            ) {
                valid = false;
                if input.has_input_content_hash()
                    && input.index.is_some()
                    && input.info_type == InputInfoType::Cached
                {
                    revalidate.push(position);
                } else {
                    *can_revalidate = false;
                    return false;
                }
            }
        }
        valid
    }

    /// Validates one input record (purge-aware).
    fn input_validity(&self, input: &InputInfo) -> InputValidity {
        let options = self.driver.options();
        let url_valid = match input.url.as_deref() {
            Some(url) => options.is_url_cache_valid(url, input.date_ms.unwrap_or(0)),
            None => true,
        };
        is_input_valid(
            input,
            self.driver.server_context().now_ms(),
            false,
            options.metadata_cache_staleness_threshold_ms(),
            url_valid,
        )
    }

    /// Rebuilds outputs for cached partitions.
    fn create_outputs_for_hit(&mut self) {
        self.outputs.clear();
        for partition in self.partitions.partition.clone() {
            if partition.optimizable {
                self.outputs
                    .push(self.output_for_cached_partition(&partition));
            } else {
                self.outputs.push(None);
            }
        }
    }

    /// Rebuilds one output from its partition (external kind).
    fn output_for_cached_partition(&self, partition: &CachedResult) -> Option<OutputResource> {
        let gurl = GoogleUrl::new(&partition.url);
        if !gurl.is_web_valid() {
            return None;
        }
        let mut namer = ps_cache::namer::ResourceNamer::new();
        if !namer.decode_ignore_hash_and_signature(gurl.leaf_with_query()) {
            return None;
        }
        let content_type =
            ps_http::content_type::extension_to_content_type(&format!(".{}", partition.extension));
        let base = gurl.all_except_leaf().to_string();
        let mut output = super::output_resource::OutputResource::new(
            self.driver.clone(),
            &base,
            &base,
            self.driver.base_url().origin(),
            namer,
            self.behavior.kind(),
        );
        output.set_type(content_type);
        Some(output)
    }

    /// Serves a metadata hit (`OutputCacheHit`).
    fn output_cache_hit(&mut self, dom: &mut Dom, write_partitions: bool) {
        for index in 0..self.partitions.partition.len() {
            if self
                .outputs
                .get(index)
                .is_some_and(|output| output.is_some())
            {
                self.mark_partition_for_render(index);
            }
        }
        self.ok_to_write_partitions = write_partitions;
        self.finalize(dom);
    }

    /// Revalidates stale inputs (`OutputCacheRevalidate`, inline).
    fn output_cache_revalidate(&mut self, dom: &mut Dom, positions: Vec<(usize, usize)>) {
        let mut ok = true;
        for (partition_index, input_position) in positions {
            let slot_index = self.partitions.partition[partition_index].input[input_position]
                .index
                .expect("validated index") as usize;
            ok &= self.revalidate_input(partition_index, input_position, slot_index);
        }
        if ok {
            self.output_cache_hit(dom, true);
        } else {
            self.output_cache_miss(dom);
        }
    }

    /// Refetches one input and extends its record on a hash match.
    fn revalidate_input(
        &mut self,
        partition_index: usize,
        input_position: usize,
        slot_index: usize,
    ) -> bool {
        let mut callback = MockLoadCallback::new();
        let mut ctx = self.driver.request_context().borrow().clone();
        self.inputs[slot_index].load(
            NotCacheablePolicy::ReportFailureIfNotCacheable,
            &mut ctx,
            &mut callback,
        );
        if !callback.success() {
            return false;
        }
        let input = &self.inputs[slot_index];
        if !input.is_valid_and_cacheable() {
            return false;
        }
        let record = &mut self.partitions.partition[partition_index].input[input_position];
        if input.contents_hash() != record.input_content_hash.clone().unwrap_or_default() {
            return false;
        }
        input.fill_in_partition_input_info(true, record);
        true
    }

    /// Handles a metadata miss (`OutputCacheMiss`, lock-free).
    fn output_cache_miss(&mut self, dom: &mut Dom) {
        self.outputs.clear();
        self.partitions.clear();
        // Locks are mute in the sync model. Fetch every slot input
        // first, then partition over the loaded inputs, then rewrite
        // each partition (`FetchInputs` -> `Activate` ->
        // `PartitionAsync` -> `Rewrite`). Held inputs conclude
        // nothing, so their contexts write nothing (C++ drops
        // uncompleted contexts at detach).
        for index in 0..self.inputs.len() {
            self.fetch_slot_input(index);
        }
        if self.inputs.iter().any(InputResource::load_pending) {
            self.ok_to_write_partitions = false;
        }
        self.partition();
        if self.partitions.partition.is_empty() {
            // Zero partitions: recheck dependency, then finalize.
            self.add_recheck_dependency();
            self.finalize(dom);
            return;
        }
        let count = self.partitions.partition.len();
        for index in 0..count {
            self.rewrite_partition(index);
            if self.was_too_busy {
                break;
            }
        }
        self.finalize(dom);
    }

    /// Runs single-input partitioning (`SingleRewriteContext::Partition`).
    fn partition(&mut self) {
        if self.slots.len() != 1 {
            return;
        }
        let (safe, reason) = self.inputs[0].is_safe_to_rewrite(false);
        if !safe {
            self.partitions.debug_message.push(reason);
            // NOTE: C++ returns true with zero partitions here (safe
            // check failed, not a partition failure); the empty table
            // flows to finalize below.
            return;
        }
        let output = match self.driver.map_output_base(self.inputs[0].url()) {
            Ok(mapped) => {
                let leaf = mapped.leaf_with_query().as_bytes().to_vec();
                let name = self.behavior.encode_output_name(&leaf);
                self.driver.create_output_resource_with_name(
                    self.behavior.id(),
                    &name,
                    &mapped,
                    &GoogleUrl::new(self.inputs[0].url()),
                    self.behavior.kind(),
                )
            }
            Err(reason) => Err(reason),
        };
        match output {
            Ok(output) => {
                let partition = self.partitions.add_partition();
                partition.input.push(InputInfo::default());
                let last = partition.input.len() - 1;
                // `AddInputInfoToPartition(kIncludeInputHash, 0, ...)`.
                partition.input[last].index = Some(0);
                self.inputs[0].fill_in_partition_input_info(true, &mut partition.input[last]);
                self.outputs.push(Some(output));
            }
            Err(reason) => {
                self.partitions.debug_message.push(reason);
            }
        }
    }

    /// Loads one slot's input (`FetchInputs`, inline).
    fn fetch_slot_input(&mut self, slot_index: usize) {
        self.fetch_slot_input_with_policy(
            slot_index,
            NotCacheablePolicy::ReportFailureIfNotCacheable,
        );
    }

    /// Loads one slot's input under a policy.
    fn fetch_slot_input_with_policy(&mut self, slot_index: usize, policy: NotCacheablePolicy) {
        // NOTE: nested reconstruction of pagespeed inputs lands with
        // serving; plain loads complete inline here.
        let input = &mut self.inputs[slot_index];
        if !(input.loaded() && input.http_status_ok()) {
            let mut callback = MockLoadCallback::new();
            let mut ctx = self.driver.request_context().borrow().clone();
            input.load(policy, &mut ctx, &mut callback);
        }
    }

    /// Rewrites one partition (`Rewrite` + `RewriteDone`).
    fn rewrite_partition(&mut self, partition_index: usize) {
        // NOTE: single inputs only; multi-input partitions plug
        // their own indexes in when combiners land.
        let result = self.behavior.rewrite_single(
            &self.driver,
            &self.inputs[0],
            self.outputs[partition_index]
                .as_mut()
                .expect("partition output"),
            &mut self.partitions.partition[partition_index],
        );
        self.rewrite_done(result, partition_index);
    }

    /// Records a rewrite outcome (`RewriteDoneImpl`).
    fn rewrite_done(&mut self, result: RewriteResult, partition_index: usize) {
        if result == RewriteResult::TooBusy {
            self.was_too_busy = true;
            return;
        }
        let optimizable = result == RewriteResult::Ok;
        // Persist disable bits back into the input records.
        let flags: Vec<(usize, bool)> = self.partitions.partition[partition_index]
            .input
            .iter()
            .map(|input| {
                let index = input.index.expect("partition index") as usize;
                (index, self.slots[index].disable_further_processing())
            })
            .collect();
        let partition = &mut self.partitions.partition[partition_index];
        for (position, (_, disabled)) in flags.iter().enumerate() {
            if *disabled {
                partition.input[position].disable_further_processing = Some(true);
            }
        }
        partition.optimizable = optimizable;
        if optimizable {
            self.mark_partition_for_render(partition_index);
        }
    }

    /// Marks a partition's slots for render (`RenderPartitionOnDetach`).
    fn mark_partition_for_render(&mut self, partition_index: usize) {
        let indexes: Vec<usize> = self.partitions.partition[partition_index]
            .input
            .iter()
            .map(|input| input.index.expect("validated index") as usize)
            .collect();
        for index in indexes {
            self.slots[index].set_was_optimized(true);
            self.render_slots[index] = true;
        }
    }

    /// Adds a forced-recheck dependency (`AddRecheckDependency`).
    fn add_recheck_dependency(&mut self) {
        let now = self.driver.server_context().now_ms();
        let mut ttl_ms = self.driver.options().implicit_cache_ttl_ms();
        if self.slots.len() == 1 {
            match self.inputs[0].fetch_response_status() {
                FetchStatus::Ok => {
                    ttl_ms = ttl_ms.max(self.inputs[0].cache_expiration_time_ms() - now);
                }
                FetchStatus::Error4xx => {
                    ttl_ms = self.driver.options().metadata_input_errors_cache_ttl_ms();
                }
                status => {
                    ttl_ms = i64::from(
                        self.driver
                            .server_context()
                            .http_cache()
                            .failure_caching_ttl_sec(status),
                    ) * 1000;
                }
            }
        }
        let force = self.partitions.add_other_dependency();
        force.info_type = InputInfoType::Cached;
        force.expiration_time_ms = Some(now + ttl_ms);
    }

    /// Writes the partition table (`WritePartition`).
    fn write_partition(&mut self) {
        // Dropped rewrites are transient; never cache them.
        if !self.ok_to_write_partitions || self.was_too_busy {
            return;
        }
        let bytes = encode_partitions(&self.partitions);
        self.driver
            .server_context()
            .metadata_cache()
            .put(&self.partition_key, &bytes);
    }

    /// Finishes the rewrite (`Finalize` + `Propagate`, inline render).
    fn finalize(&mut self, dom: &mut Dom) {
        self.rewrite_done = true;
        self.write_partition();
        let render = self
            .behavior
            .policy_permits_rendering(&self.driver, &self.outputs);
        self.propagate(dom, render);
        if render && !self.was_too_busy {
            self.behavior
                .note_render(&self.driver, dom, &self.partitions, &self.slots);
        }
    }

    /// Renders slots from their partitions (`Propagate`).
    fn propagate(&mut self, dom: &mut Dom, render_slots: bool) {
        // NOTE: successor chaining lands with multi-filter flows.
        let base = self.driver.base_url().clone();
        let options = self.driver.options().clone();
        for partition_index in 0..self.partitions.partition.len() {
            let inputs: Vec<usize> = self.partitions.partition[partition_index]
                .input
                .iter()
                .map(|input| input.index.expect("validated index") as usize)
                .collect();
            let relocatable = self.partitions.partition[partition_index].url_relocatable;
            // Custom partition rendering (images) runs first and may
            // suppress the default slot rendering below.
            let custom = render_slots
                && relocatable
                && !self.was_too_busy
                && self.behavior.render_partition(
                    &self.driver,
                    dom,
                    &base,
                    &options,
                    &self.partitions.partition[partition_index].clone(),
                    &self.slots,
                    &self.outputs,
                    &inputs,
                );
            for slot_index in inputs {
                if !self.render_slots[slot_index] {
                    continue;
                }
                // Swap in the output snapshot, then render relocatable URLs.
                if let Some(output) = self.outputs[partition_index].clone() {
                    self.slots[slot_index].set_resource(Rc::new(output));
                }
                if render_slots && relocatable && !self.was_too_busy && !custom {
                    self.slots[slot_index].render(dom, &base, &options);
                }
            }
        }
        if render_slots && !self.slots.is_empty() {
            // Table debug messages trail the first slot's element.
            if let Some(element) = self.slots[0].element() {
                let messages = self.partitions.debug_message.clone();
                self.driver.insert_debug_comments(dom, &messages, element);
            }
            // Partition messages trail their first input's slot (slot
            // 0 when the partition holds debug info only).
            for partition in self.partitions.partition.clone() {
                if partition.debug_message.is_empty() {
                    continue;
                }
                let slot_index = partition
                    .input
                    .first()
                    .and_then(|input| input.index)
                    .map(|index| index as usize)
                    .unwrap_or(0);
                if let Some(slot) = self.slots.get(slot_index) {
                    if let Some(element) = slot.element() {
                        self.driver
                            .insert_debug_comments(dom, &partition.debug_message, element);
                    }
                }
            }
        }
    }

    /// Serves a decoded output (`Fetch`, sync completion): prepares
    /// inputs, consults metadata, reconstructs on miss, and drives
    /// `fetch` to `Done`. Returns whether bytes were served.
    pub fn fetch(&mut self, output: OutputResource, fetch: &mut dyn AsyncFetch) -> bool {
        if !self.prepare_fetch(output) {
            fetch.response_headers_mut().set_status_and_reason(404);
            fetch.handle_done(false);
            return false;
        }
        self.start_fetch(fetch)
    }

    /// Decodes input URLs and builds fetch slots (`PrepareFetch`).
    fn prepare_fetch(&mut self, output: OutputResource) -> bool {
        {
            let hasher = self.driver.server_context().hasher();
            let eventual = output
                .full_name()
                .eventual_size(&**hasher, self.driver.signature_length());
            let max_segment = self.driver.options().max_url_segment_size();
            if max_segment >= 0 && eventual > max_segment as usize {
                return false;
            }
        }
        let Some(urls) = self.decode_fetch_urls(&output) else {
            return false;
        };
        // A name decoding to no URLs names nothing to reconstruct.
        if urls.is_empty() {
            return false;
        }
        for url_text in urls {
            let url = GoogleUrl::new(&url_text);
            if !url.is_web_valid() {
                return false;
            }
            // NOTE: absolute-URL proxy checks land with the proxy namer.
            if !self.driver.matches_base_url(&url) {
                return false;
            }
            let (resource, _authorized) = self.driver.create_input_resource(
                &url,
                InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
                IntendedFor::General,
                InputRole::Reconstruction,
            );
            let Some(mut input) = resource else {
                return false;
            };
            input.set_is_background_fetch(false);
            self.add_fetch_slot(input);
        }
        self.set_partition_key();
        let requested_hash = output.full_name().hash().to_string();
        let original_url = output.url().to_string();
        self.fetch = Some(FetchState {
            requested_hash,
            original_url,
            success: false,
        });
        self.fetch_output = Some(output);
        true
    }

    /// Resolves an output's encoded inputs (`DecodeFetchUrls`,
    /// single-step only: chained re-encoding needs nested serving).
    fn decode_fetch_urls(&self, output: &OutputResource) -> Option<Vec<String>> {
        let name = output.full_name().name();
        let urls = self.behavior.decode_output_name(name.as_bytes())?;
        let base = GoogleUrl::new(&output.decoded_base());
        let mut out = Vec::new();
        for url in urls {
            let text = String::from_utf8_lossy(&url);
            out.push(GoogleUrl::resolve(&base, &text).spec().to_string());
        }
        Some(out)
    }

    /// Opens a fetch rewrite (`StartFetch`, locks mute): metadata
    /// consult first, reconstruction for on-the-fly outputs.
    fn start_fetch(&mut self, fetch: &mut dyn AsyncFetch) -> bool {
        if self.behavior.kind() == OutputResourceKind::OnTheFly {
            return self.start_fetch_reconstruction(fetch);
        }
        let lookup = self
            .driver
            .server_context()
            .metadata_cache()
            .get(&self.partition_key);
        let tables = if lookup.state == KeyState::Available {
            lookup
                .value
                .and_then(|bytes| decode_partitions(&bytes))
                .filter(|tables| {
                    self.other_dependencies_valid(tables)
                        && self.slot_indexes_valid(tables)
                        && tables
                            .partition
                            .iter()
                            .all(|partition| self.fetch_partition_fresh(partition))
                })
        } else {
            None
        };
        self.fetch_cache_done(fetch, tables)
    }

    /// Whether a metadata partition serves a fetch as-is (every
    /// input fresh; stale entries reconstruct instead).
    fn fetch_partition_fresh(&self, partition: &CachedResult) -> bool {
        partition
            .input
            .iter()
            .all(|input| self.input_validity(input) == InputValidity::Valid)
    }

    /// Serves from metadata or falls into reconstruction
    /// (`FetchCacheDone`).
    fn fetch_cache_done(
        &mut self,
        fetch: &mut dyn AsyncFetch,
        tables: Option<OutputPartitions>,
    ) -> bool {
        if let Some(tables) = tables {
            if tables.partition.len() == 1 && tables.partition[0].optimizable {
                if let Some(output) = self.output_for_cached_partition(&tables.partition[0]) {
                    let requested = self.fetch_hash().to_string();
                    if self.behavior.fail_on_hash_mismatch()
                        && output.full_name().hash() != requested
                    {
                        self.partitions = tables;
                        return self.fail_for_hash_mismatch(fetch);
                    }
                    let key = output.http_cache_key();
                    self.partitions = tables;
                    if self.fetch_try_fallback(&key, fetch) {
                        return true;
                    }
                    return self.start_fetch_reconstruction(fetch);
                }
            }
            // Non-optimizable (or unbuildable): try the original.
            if self.can_fetch_fallback_to_original(false) {
                let url = self.inputs[0].url().to_string();
                self.partitions = tables;
                if self.fetch_try_fallback(&url, fetch) {
                    return true;
                }
            } else {
                self.partitions = tables;
            }
        }
        self.start_fetch_reconstruction(fetch)
    }

    /// Borrows the requested hash (valid after [`prepare_fetch`](Self::prepare_fetch)).
    fn fetch_hash(&self) -> &str {
        &self.fetch.as_ref().expect("fetch state").requested_hash
    }

    /// Whether the original may substitute
    /// (`CanFetchFallbackToOriginal`).
    fn can_fetch_fallback_to_original(&self, emergency: bool) -> bool {
        if !self.behavior.optimization_only() && !emergency {
            return false;
        }
        if self.behavior.fail_on_hash_mismatch() {
            return false;
        }
        self.slots.len() == 1
    }

    /// Serves a cached fallback with shortened headers
    /// (`FetchTryFallback` + `FetchFallbackDoneImpl`).
    fn fetch_try_fallback(&self, url: &str, fetch: &mut dyn AsyncFetch) -> bool {
        use ps_http::http_cache::FindStatus;
        use ps_http::response::{RequestProperties, VaryOption};
        let mut ctx = self.driver.request_context().borrow().clone();
        let found = self.driver.server_context().http_cache().find(
            url,
            self.driver.cache_fragment(),
            RequestProperties::default(),
            VaryOption::IgnoreVaryOnResources,
            &|_| true,
            -1,
            &mut ctx,
            self.driver.server_context().now_ms(),
        );
        if found.status != FindStatus::Found {
            return false;
        }
        let Some(headers) = found.headers else {
            return false;
        };
        if headers.status_code() != 200 {
            return false;
        }
        let had_public = headers.has_value(ps_http::names::header::CACHE_CONTROL, "public");
        let mut fixed = headers.clone();
        self.fix_fetch_fallback_headers(&mut fixed);
        fetch.response_headers_mut().copy_from(&fixed);
        if had_public {
            Self::merge_cache_control_public(fetch.response_headers_mut());
        }
        self.adjust_cache_control(fetch);
        let ok = self.send_fallback_response(&found.body, fetch);
        fetch.handle_done(ok);
        true
    }

    /// Shortens fallback headers to input TTLs
    /// (`FixFetchFallbackHeaders`).
    fn fix_fetch_fallback_headers(&self, headers: &mut ps_http::response::ResponseHeaders) {
        if headers.sanitize() {
            headers.compute_caching();
        }
        let date_ms = headers.date_ms();
        let mut min_expiry_ms = headers.cache_ttl_ms() + date_ms;
        for partition in &self.partitions.partition {
            for input in &partition.input {
                if input.info_type == InputInfoType::Cached {
                    if let Some(expiry_ms) = input.expiration_time_ms {
                        if expiry_ms > 0 {
                            min_expiry_ms = min_expiry_ms.min(expiry_ms);
                        }
                    }
                }
            }
        }
        let mut ttl_ms = min_expiry_ms - date_ms;
        let suffix = if self
            .driver
            .options()
            .publicly_cache_mismatched_hashes_experimental()
        {
            ""
        } else {
            ttl_ms = ttl_ms.min(self.driver.options().implicit_cache_ttl_ms());
            ",private"
        };
        headers.set_date_and_caching(date_ms, ttl_ms, suffix);
        headers
            .headers_mut()
            .replace("X-Content-Type-Options", "nosniff");
        headers
            .headers_mut()
            .remove_all(ps_http::names::header::ETAG);
        headers.compute_caching();
        self.behavior
            .extra_fetch_fallback_headers(&self.driver, &self.inputs, headers);
    }

    /// Merges `public` unless governed (`SetCacheControlPublic`).
    fn merge_cache_control_public(headers: &mut ps_http::response::ResponseHeaders) {
        use ps_http::names::header;
        let current = headers
            .headers()
            .lookup(header::CACHE_CONTROL)
            .unwrap_or_default();
        if current.iter().any(|value| {
            ["private", "public", "no-cache", "no-store"]
                .iter()
                .any(|directive| value.eq_ignore_ascii_case(directive))
        }) {
            return;
        }
        let mut merged = current.join(", ");
        if merged.is_empty() {
            merged.push_str("public");
        } else {
            merged.push_str(", public");
        }
        headers
            .headers_mut()
            .replace(header::CACHE_CONTROL, &merged);
    }

    /// Applies input cache control plus the Google-CDN fix
    /// (`AdjustCacheControl`).
    fn adjust_cache_control(&self, fetch: &mut dyn AsyncFetch) {
        self.driver
            .server_context()
            .apply_input_cache_control(&self.inputs, fetch.response_headers_mut());
        let request = fetch.request_headers().clone();
        ps_http::fetch::fix_cache_control_for_google_cache(&request, fetch.response_headers_mut());
    }

    /// Sends fallback bytes past the content-type gate
    /// (`SendFallbackResponse`, behavior hook).
    fn send_fallback_response(&self, contents: &[u8], fetch: &mut dyn AsyncFetch) -> bool {
        let original_url = self
            .fetch
            .as_ref()
            .map(|state| state.original_url.clone())
            .unwrap_or_default();
        self.behavior.send_fallback_response(
            &self.driver,
            &self.inputs,
            &original_url,
            contents,
            fetch,
        )
    }

    /// Rejects a wrong-hash serve (`FailForHashMismatch`): 404 with
    /// the mismatch body, still `Done(true)`.
    fn fail_for_hash_mismatch(&self, fetch: &mut dyn AsyncFetch) -> bool {
        fetch.response_headers_mut().set_status_and_reason(404);
        fetch.headers_complete();
        fetch.handle_write(HASH_MISMATCH_MESSAGE.as_bytes());
        fetch.handle_done(true);
        true
    }

    /// Reconstructs the output inline (`StartFetchReconstruction` +
    /// `StartRewriteForFetch`, deadlines and budgets mute).
    fn start_fetch_reconstruction(&mut self, fetch: &mut dyn AsyncFetch) -> bool {
        self.partitions.clear();
        // Fetches load inputs even when uncacheable (top-level
        // fetch rewrites may still fall back to them).
        for index in 0..self.inputs.len() {
            self.fetch_slot_input_with_policy(index, NotCacheablePolicy::LoadEvenIfNotCacheable);
        }
        // Fake single partition over the loaded inputs.
        let mut ok_to_rewrite = true;
        {
            let partition = self.partitions.add_partition();
            let no_transform_off = self.driver.options().disable_rewrite_on_no_transform();
            for (index, input) in self.inputs.iter().enumerate() {
                let transform_ok = !(no_transform_off
                    && input
                        .response_headers()
                        .has_cache_control_directive("no-transform"));
                if input.loaded() && input.http_status_ok() && transform_ok {
                    let include_hash = self.behavior.kind() != OutputResourceKind::OnTheFly;
                    partition.input.push(InputInfo::default());
                    let last = partition.input.len() - 1;
                    let slot_index = i32::try_from(index).unwrap_or(i32::MAX);
                    partition.input[last].index = Some(slot_index);
                    input.fill_in_partition_input_info(include_hash, &mut partition.input[last]);
                } else {
                    ok_to_rewrite = false;
                    break;
                }
            }
        }
        if ok_to_rewrite {
            let result = {
                let output = self.fetch_output.as_mut().expect("fetch output");
                let partition = &mut self.partitions.partition[0];
                self.behavior
                    .rewrite_single(&self.driver, &self.inputs[0], output, partition)
            };
            self.rewrite_done(result, 0);
            if let Some(state) = self.fetch.as_mut() {
                state.success = result == RewriteResult::Ok;
            }
        } else {
            self.partitions.partition[0].input.clear();
            self.add_recheck_dependency();
            self.rewrite_done(RewriteResult::Failed, 0);
            if let Some(state) = self.fetch.as_mut() {
                state.success = false;
            }
        }
        self.fetch_done(fetch)
    }

    /// Whether a fetch stores what HTML would (`FetchWritesPartition`).
    fn fetch_writes_partition(&self) -> bool {
        if self.slots.len() != 1 {
            return false;
        }
        if !self.inputs[0].is_authorized_domain() {
            return false;
        }
        // The output an HTML rewrite of this input creates, through
        // the same domain mapping.
        let canonical = match self.driver.map_output_base(self.inputs[0].url()) {
            Ok(mapped) => {
                let leaf = mapped.leaf_with_query().as_bytes().to_vec();
                let name = self.behavior.encode_output_name(&leaf);
                self.driver.create_output_resource_with_name(
                    self.behavior.id(),
                    &name,
                    &mapped,
                    &GoogleUrl::new(self.inputs[0].url()),
                    self.behavior.kind(),
                )
            }
            Err(_) => return false,
        };
        let Ok(mut canonical) = canonical else {
            return false;
        };
        // Give it the hash, extension and signature of what the fetch
        // produced, so that the two URLs agree exactly when the
        // directory, domain, shard and encoded name do.
        let Some(fetched) = self.fetch_output.as_ref() else {
            return false;
        };
        canonical.sync_naming_from(fetched.full_name());
        canonical.url() == fetched.url()
    }

    /// Serves the reconstruction (`FetchContext::FetchDone`).
    fn fetch_done(&mut self, fetch: &mut dyn AsyncFetch) -> bool {
        self.rewrite_done = true;
        if !self.fetch_writes_partition() {
            self.ok_to_write_partitions = false;
        }
        self.write_partition();
        let success = self.fetch.as_ref().expect("fetch state").success;
        if success {
            let (hash, requested) = {
                let output = self.fetch_output.as_ref().expect("fetch output");
                let state = self.fetch.as_ref().expect("fetch state");
                (
                    output.full_name().hash().to_string(),
                    state.requested_hash.clone(),
                )
            };
            if hash == requested {
                let (headers, body) = {
                    let output = self.fetch_output.as_ref().expect("fetch output");
                    (output.response_headers().clone(), output.body().to_vec())
                };
                fetch.response_headers_mut().copy_from(&headers);
                self.adjust_cache_control(fetch);
                fetch.headers_complete();
                let ok = fetch.handle_write(&body);
                fetch.handle_done(ok);
                return ok;
            }
            if self.behavior.fail_on_hash_mismatch() {
                return self.fail_for_hash_mismatch(fetch);
            }
            // A rewritten hash differing from the request serves with
            // fallback (shortened) headers.
            let (headers, body) = {
                let output = self.fetch_output.as_ref().expect("fetch output");
                (output.response_headers().clone(), output.body().to_vec())
            };
            let mut fixed = headers;
            self.fix_fetch_fallback_headers(&mut fixed);
            let code = fixed.status_code();
            fetch.response_headers_mut().copy_from(&fixed);
            self.adjust_cache_control(fetch);
            let ok = self.send_fallback_response(&body, fetch);
            let ok = ok && code == 200;
            fetch.handle_done(ok);
            return ok;
        }
        // Rewrite failed: fall back to the original when allowed.
        if self.can_fetch_fallback_to_original(true) && self.inputs[0].http_status_ok() {
            let mut headers = self.inputs[0].response_headers().clone();
            self.fix_fetch_fallback_headers(&mut headers);
            fetch.response_headers_mut().copy_from(&headers);
            self.adjust_cache_control(fetch);
            let body = self.inputs[0].contents();
            let ok = self.send_fallback_response(&body, fetch);
            fetch.handle_done(ok);
            return ok;
        }
        fetch.response_headers_mut().set_status_and_reason(404);
        fetch.headers_complete();
        fetch.handle_done(false);
        false
    }
}

/// Per-filter inline behavior (the `InlineRewriteContext`
/// subclass surface: identity plus the inline verdict/render).
pub trait InlineBehavior {
    /// Filter id prefix.
    fn id(&self) -> &'static str;

    /// URL segment codec.
    fn encoder(&self) -> &'static dyn UrlSegmentEncoder {
        &DEFAULT_ENCODER
    }

    /// Input role for resource creation.
    fn input_role(&self) -> InputRole;

    /// Authorization policy (`AllowUnauthorizedDomain`).
    fn allow_unauthorized_domain(&self, _driver: &Rc<ResourceDriver>) -> InlineAuthorizationPolicy {
        InlineAuthorizationPolicy::InlineOnlyAuthorizedResources
    }

    /// Whether creation targets inlining (`IntendedForInlining`).
    fn intended_for_inlining(&self) -> bool {
        false
    }

    /// Partition-key suffix (`CacheKeySuffix`, e.g. charsets).
    fn cache_key_suffix(&self) -> String {
        String::new()
    }

    /// Whether rendering may proceed (`PolicyPermitsRendering`).
    fn policy_permits_rendering(&self) -> bool {
        true
    }

    /// Creates the input (`CreateResource`): the resource plus
    /// whether its domain is authorized.
    fn create_resource(
        &self,
        driver: &Rc<ResourceDriver>,
        url: &GoogleUrl,
    ) -> (Option<InputResource>, bool) {
        let policy = self.allow_unauthorized_domain(driver);
        let intended_for = if self.intended_for_inlining() {
            IntendedFor::Inlining
        } else {
            IntendedFor::General
        };
        driver.create_input_resource(url, policy, intended_for, self.input_role())
    }

    /// Whether an input inlines (`ShouldInline`), with the reason on refusal.
    fn should_inline(
        &self,
        driver: &Rc<ResourceDriver>,
        input: &InputResource,
        reason: &mut String,
    ) -> bool;

    /// Renders inlined bytes into the element (`RenderInline`).
    #[allow(clippy::too_many_arguments)]
    fn render_inline(
        &self,
        driver: &Rc<ResourceDriver>,
        input: &InputResource,
        partition: &CachedResult,
        base_url: &GoogleUrl,
        dom: &mut Dom,
        element: crate::element::NodeId,
    );

    /// Runs when rendering carries no inlined data (CSS drops its
    /// LSC attributes there; `Context::Render`).
    fn render_without_inline_data(
        &self,
        _driver: &Rc<ResourceDriver>,
        _dom: &mut Dom,
        _element: crate::element::NodeId,
    ) {
    }
}

/// One inline rewrite (`InlineRewriteContext`, sync completion).
///
/// No outputs are ever created: the partition carries the inlined
/// bytes (or the refusal reason), and rendering swaps the element.
/// Fetch/serving modes do not exist (C++ `kind()` FATALs).
pub struct InlineRewriteContext<B: InlineBehavior> {
    driver: Rc<ResourceDriver>,
    behavior: B,
    slot: Option<ContextSlot>,
    input: Option<InputResource>,
    partitions: OutputPartitions,
    partition_key: String,
    element: crate::element::NodeId,
    base_url: GoogleUrl,
    rewrite_done: bool,
    ok_to_write_partitions: bool,
}

impl<B: InlineBehavior> InlineRewriteContext<B> {
    /// Builds a context over a driver, element, and base URL.
    pub fn new(
        driver: Rc<ResourceDriver>,
        behavior: B,
        element: crate::element::NodeId,
        base_url: GoogleUrl,
    ) -> Self {
        InlineRewriteContext {
            driver,
            behavior,
            slot: None,
            input: None,
            partitions: OutputPartitions::default(),
            partition_key: String::new(),
            element,
            base_url,
            rewrite_done: false,
            ok_to_write_partitions: true,
        }
    }

    /// Creates the input and starts (`StartInlining`): false (with a
    /// debug comment when warranted) when no input exists.
    pub fn start_inlining(&mut self, dom: &mut Dom, attribute: usize, src: Option<&[u8]>) -> bool {
        let Some(value) = src else {
            self.driver.insert_debug_comment(
                dom,
                "Following resource not rewritten because its src attribute cannot be decoded",
                self.element,
            );
            return false;
        };
        let url = GoogleUrl::resolve(self.driver.base_url(), &String::from_utf8_lossy(value));
        if !url.is_web_valid() {
            return false;
        }
        let (resource, authorized) = self.behavior.create_resource(&self.driver, &url);
        let Some(input) = resource else {
            if !authorized {
                self.driver.insert_unauthorized_domain_debug_comment(
                    dom,
                    &String::from_utf8_lossy(value),
                    self.behavior.input_role(),
                    self.element,
                );
            }
            return false;
        };
        let handle: Rc<dyn Resource> = Rc::new(input.clone());
        let slot = self.driver.get_slot(
            Some(handle),
            dom,
            self.element,
            attribute,
            self.behavior.id(),
        );
        self.slot = Some(ContextSlot::Html(slot));
        self.input = Some(input);
        self.start(dom);
        true
    }

    /// Runs the rewrite to completion inline.
    pub fn start(&mut self, dom: &mut Dom) {
        if !self.driver.can_rewrite_resources() {
            self.rewrite_done = true;
            return;
        }
        if self
            .slot
            .as_ref()
            .is_some_and(ContextSlot::disable_further_processing)
        {
            self.rewrite_done = true;
            self.propagate(dom, false);
            return;
        }
        self.set_partition_key();
        match self.lookup_partitions() {
            CacheDecision::Hit => self.output_cache_hit(dom),
            CacheDecision::Revalidate(positions) => self.output_cache_revalidate(dom, positions),
            CacheDecision::Miss => self.output_cache_miss(dom),
        }
    }

    /// Computes the partition key (with the behavior suffix).
    fn set_partition_key(&mut self) {
        let signature = self.driver.partition_signature_hash();
        let encoding = self.behavior.encoder().encode(&[Vec::new()]);
        let suffix = format!(
            "{}@_{}",
            String::from_utf8_lossy(&encoding),
            self.behavior.cache_key_suffix()
        );
        let url_key = self.input.as_ref().map_or_else(
            || "empty".to_string(),
            |input| input.cache_key().to_string(),
        );
        self.partition_key = format!(
            "rname/{}_{}/{url_key}@{suffix}",
            self.behavior.id(),
            signature
        );
    }

    /// Looks up and validates the partition table.
    fn lookup_partitions(&mut self) -> CacheDecision {
        let lookup = self
            .driver
            .server_context()
            .metadata_cache()
            .get(&self.partition_key);
        if lookup.state != KeyState::Available {
            self.driver
                .server_context()
                .variable("rewrite_cached_output_misses")
                .add(1);
            return CacheDecision::Miss;
        }
        let bytes = lookup.value.unwrap_or_default();
        let Some(tables) = decode_partitions(&bytes) else {
            self.driver
                .server_context()
                .variable("rewrite_cached_output_misses")
                .add(1);
            return CacheDecision::Miss;
        };
        if !self.other_dependencies_valid(&tables) || !self.slot_indexes_valid(&tables) {
            self.driver
                .server_context()
                .variable("rewrite_cached_output_misses")
                .add(1);
            return CacheDecision::Miss;
        }
        let mut revalidate = Vec::new();
        let mut can_revalidate = true;
        let mut all_valid = true;
        for (partition_index, partition) in tables.partition.iter().enumerate() {
            let mut partition_positions = Vec::new();
            let mut partition_can = true;
            if !self.partition_valid(partition, &mut partition_can, &mut partition_positions) {
                all_valid = false;
                can_revalidate = can_revalidate && partition_can;
                revalidate.extend(
                    partition_positions
                        .into_iter()
                        .map(|input_position| (partition_index, input_position)),
                );
            }
        }
        if all_valid {
            self.partitions = tables;
            CacheDecision::Hit
        } else if can_revalidate {
            self.partitions = tables;
            CacheDecision::Revalidate(revalidate)
        } else {
            self.driver
                .server_context()
                .variable("rewrite_cached_output_misses")
                .add(1);
            CacheDecision::Miss
        }
    }

    /// Classifies an input record (`IsInputValid` + purge check).
    fn input_validity(&self, input: &InputInfo) -> InputValidity {
        let options = self.driver.options();
        let url_valid = match input.url.as_deref() {
            Some(url) => options.is_url_cache_valid(url, input.date_ms.unwrap_or(0)),
            None => true,
        };
        is_input_valid(
            input,
            self.driver.server_context().now_ms(),
            false,
            options.metadata_cache_staleness_threshold_ms(),
            url_valid,
        )
    }

    /// Validates forced-recheck dependencies.
    fn other_dependencies_valid(&self, tables: &OutputPartitions) -> bool {
        tables.other_dependency.iter().all(|input| {
            matches!(
                self.input_validity(input),
                InputValidity::Valid | InputValidity::Stale
            )
        })
    }

    /// Rejects entries naming missing slots.
    fn slot_indexes_valid(&self, tables: &OutputPartitions) -> bool {
        let slots = usize::from(self.slot.is_some());
        tables.partition.iter().all(|partition| {
            partition.input.iter().all(|input| {
                input
                    .index
                    .is_some_and(|index| index >= 0 && (index as usize) < slots)
            })
        })
    }

    /// Validates one partition, collecting revalidation positions.
    fn partition_valid(
        &self,
        partition: &CachedResult,
        partition_can: &mut bool,
        positions: &mut Vec<usize>,
    ) -> bool {
        let mut valid = true;
        for (input_position, input) in partition.input.iter().enumerate() {
            if !matches!(
                self.input_validity(input),
                InputValidity::Valid | InputValidity::Stale
            ) {
                valid = false;
                if input.has_input_content_hash()
                    && input.index.is_some()
                    && input.info_type == InputInfoType::Cached
                {
                    positions.push(input_position);
                } else {
                    *partition_can = false;
                    return false;
                }
            }
        }
        valid
    }

    /// Serves a metadata hit.
    fn output_cache_hit(&mut self, dom: &mut Dom) {
        self.ok_to_write_partitions = false;
        self.finalize(dom);
    }

    /// Revalidates stale inputs, else misses.
    fn output_cache_revalidate(&mut self, dom: &mut Dom, positions: Vec<(usize, usize)>) {
        let mut ok = true;
        for (partition_index, input_position) in positions {
            ok &= self.revalidate_input(partition_index, input_position);
        }
        if ok {
            self.ok_to_write_partitions = true;
            self.finalize(dom);
        } else {
            self.output_cache_miss(dom);
        }
    }

    /// Refetches the input and extends its record on a hash match.
    fn revalidate_input(&mut self, partition_index: usize, input_position: usize) -> bool {
        let mut callback = MockLoadCallback::new();
        let mut ctx = self.driver.request_context().borrow().clone();
        let Some(input) = self.input.as_mut() else {
            return false;
        };
        input.load(
            NotCacheablePolicy::ReportFailureIfNotCacheable,
            &mut ctx,
            &mut callback,
        );
        if !callback.success() {
            return false;
        }
        let Some(input) = self.input.as_ref() else {
            return false;
        };
        if !input.is_valid_and_cacheable() {
            return false;
        }
        let record = &mut self.partitions.partition[partition_index].input[input_position];
        if input.contents_hash() != record.input_content_hash.clone().unwrap_or_default() {
            return false;
        }
        input.fill_in_partition_input_info(false, record);
        self.ok_to_write_partitions = true;
        true
    }

    /// Fetches, partitions, and rewrites on a miss.
    fn output_cache_miss(&mut self, dom: &mut Dom) {
        self.partitions.clear();
        if let Some(input) = self.input.as_mut() {
            if !(input.loaded() && input.http_status_ok()) {
                let mut callback = MockLoadCallback::new();
                let mut ctx = self.driver.request_context().borrow().clone();
                input.load(
                    NotCacheablePolicy::ReportFailureIfNotCacheable,
                    &mut ctx,
                    &mut callback,
                );
            }
        }
        if self.input.as_ref().is_some_and(InputResource::load_pending) {
            self.ok_to_write_partitions = false;
        }
        self.partition();
        // No output resource exists, so the rewrite always "fails"
        // structurally; the bytes (or the reason) ride the partition.
        if self.partitions.partition[0].inlined_data.is_some() {
            if let Some(slot) = self.slot.as_ref() {
                slot.set_was_optimized(true);
                slot.set_disable_further_processing(true);
            }
        }
        self.partitions.partition[0].optimizable = false;
        self.finalize(dom);
    }

    /// Builds the inline partition (`Partition`, hash omitted).
    fn partition(&mut self) {
        self.partitions.add_partition();
        let Some(input) = self.input.clone() else {
            return;
        };
        let (safe, reason) = input.is_safe_to_rewrite(false);
        if !safe {
            self.partitions.debug_message.push(reason);
            self.add_recheck_dependency();
            return;
        }
        let partition = &mut self.partitions.partition[0];
        partition.input.push(InputInfo::default());
        let last = partition.input.len() - 1;
        partition.input[last].index = Some(0);
        input.fill_in_partition_input_info(false, &mut partition.input[last]);
        let mut reason = String::new();
        if self
            .behavior
            .should_inline(&self.driver, &input, &mut reason)
        {
            let partition = &mut self.partitions.partition[0];
            partition.inlined_data = Some(input.contents());
        } else {
            let partition = &mut self.partitions.partition[0];
            partition.debug_message.push(reason);
        }
    }

    /// Adds a forced-recheck dependency (`AddRecheckDependency`).
    fn add_recheck_dependency(&mut self) {
        let now = self.driver.server_context().now_ms();
        let mut ttl_ms = self.driver.options().implicit_cache_ttl_ms();
        if let Some(input) = self.input.as_ref() {
            match input.fetch_response_status() {
                FetchStatus::Ok => {
                    ttl_ms = ttl_ms.max(input.cache_expiration_time_ms() - now);
                }
                FetchStatus::Error4xx => {
                    ttl_ms = self.driver.options().metadata_input_errors_cache_ttl_ms();
                }
                status => {
                    ttl_ms = i64::from(
                        self.driver
                            .server_context()
                            .http_cache()
                            .failure_caching_ttl_sec(status),
                    ) * 1000;
                }
            }
        }
        let force = self.partitions.add_other_dependency();
        force.info_type = InputInfoType::Cached;
        force.expiration_time_ms = Some(now + ttl_ms);
    }

    /// Writes the partition table (`WritePartition`).
    fn write_partition(&mut self) {
        if !self.ok_to_write_partitions {
            return;
        }
        let bytes = encode_partitions(&self.partitions);
        self.driver
            .server_context()
            .metadata_cache()
            .put(&self.partition_key, &bytes);
    }

    /// Finishes the rewrite: write, propagate, render inline.
    fn finalize(&mut self, dom: &mut Dom) {
        self.rewrite_done = true;
        self.write_partition();
        let render = self.behavior.policy_permits_rendering();
        self.propagate(dom, render);
        if self.partitions.partition.is_empty()
            || self.partitions.partition[0].inlined_data.is_none()
        {
            let element = self.element;
            self.behavior
                .render_without_inline_data(&self.driver, dom, element);
        }
        if render
            && self.partitions.partition.len() == 1
            && self.partitions.partition[0].inlined_data.is_some()
            && !self
                .slot
                .as_ref()
                .is_some_and(ContextSlot::should_delete_element)
        {
            if let Some(slot) = self.slot.as_ref() {
                slot.set_disable_rendering();
            }
            let partition = self.partitions.partition[0].clone();
            if let Some(input) = self.input.clone() {
                self.behavior.render_inline(
                    &self.driver,
                    &input,
                    &partition,
                    &self.base_url,
                    dom,
                    self.element,
                );
            }
        }
    }

    /// Emits debug comments (`Propagate` without slot rendering).
    fn propagate(&mut self, dom: &mut Dom, render_slots: bool) {
        if render_slots {
            if let Some(element) = self.slot.as_ref().and_then(ContextSlot::element) {
                let messages = self.partitions.debug_message.clone();
                self.driver.insert_debug_comments(dom, &messages, element);
            }
            for partition in self.partitions.partition.clone() {
                if partition.debug_message.is_empty() {
                    continue;
                }
                if let Some(element) = self.slot.as_ref().and_then(ContextSlot::element) {
                    self.driver
                        .insert_debug_comments(dom, &partition.debug_message, element);
                }
            }
        }
    }
}
