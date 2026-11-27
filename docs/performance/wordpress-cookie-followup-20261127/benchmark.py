"""Compile and compare the actual old/current cookie modules, without service traffic."""
import pathlib
import subprocess
import tempfile

out = pathlib.Path(__file__).resolve().parent
root = out.parents[2]
main = r'''
use std::{alloc::{GlobalAlloc, Layout, System}, sync::atomic::{AtomicUsize, Ordering}, time::Instant};
struct Alloc;
static CALLS: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Alloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 { CALLS.fetch_add(1, Ordering::Relaxed); System.alloc(l) }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) { System.dealloc(p,l) }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 { CALLS.fetch_add(1, Ordering::Relaxed); System.realloc(p,l,n) }
}
#[global_allocator] static ALLOC: Alloc = Alloc;
#[path="cookies.rs"] mod cookies;
fn main() {
    let empty = cookies::CookiePatternSet::new();
    let policy = cookies::CookiePolicy { common_enabled: true, ignore: &empty, bypass: &empty };
    println!("fixture,ns_per_request,allocations_per_request,allowed");
    for (fixture, cookie) in [("analytics", "_ga=one; _gid=two; _hjSession_9=three; _fbp=four"), ("expanded", "wordpress_test_cookie=WP; tk_qs=x; wp_lang=en_US; trx_addons_is_retina=1"), ("protected", "_ga=x; PHPSESSID=secret")] {
        let headers = [("Cookie", cookie)];
        for _ in 0..1000 { std::hint::black_box(cookies::evaluate_cookie_headers(&headers, &policy)); }
        let mut samples = [0.; 5];
        let mut alloc = 0;
        for t in &mut samples {
            CALLS.store(0, Ordering::Relaxed);
            let start = Instant::now();
            for _ in 0..100_000 { std::hint::black_box(cookies::evaluate_cookie_headers(std::hint::black_box(&headers), &policy)); }
            *t = start.elapsed().as_nanos() as f64 / 100_000.;
            alloc = CALLS.load(Ordering::Relaxed);
        }
        samples.sort_by(f64::total_cmp);
        let allowed = cookies::evaluate_cookie_headers(&headers, &policy).allowed();
        println!("{fixture},{:.3},{:.3},{allowed}", samples[2], alloc as f64 / 100_000.);
    }
}
'''
with tempfile.TemporaryDirectory(prefix='ps-cookie-bench-') as temp:
    work = pathlib.Path(temp)
    (work / 'main.rs').write_text(main)
    bins = {}
    for version in ('before', 'after'):
        source = subprocess.check_output(['git', 'show', 'd7ad0f8:crates/ps-config/src/cookies.rs'], cwd=root, text=True) if version == 'before' else (root / 'crates/ps-config/src/cookies.rs').read_text()
        (work / 'cookies.rs').write_text(source)
        binary = work / version
        subprocess.run(['rustc', '--edition=2021', '-O', '-Awarnings', str(work / 'main.rs'), '-o', str(binary)], check=True)
        bins[version] = binary
    for trial in range(3):
        for version in (('before', 'after') if trial % 2 == 0 else ('after', 'before')):
            result = subprocess.check_output([str(bins[version])], text=True)
            for line in result.splitlines()[1:]:
                fixture, elapsed, allocations, allowed = line.split(',')
                assert allowed == ('false' if fixture == 'protected' or (version == 'before' and fixture == 'expanded') else 'true')
                if version == 'after' and fixture != 'protected':
                    assert float(allocations) == 0, line
            (out / f'{version}-trial-{trial + 1}.csv').write_text(result)
            print(version, trial + 1, result, flush=True)
