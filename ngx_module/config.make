# Pagespeed Rust make hook: `make modules` rebuilds the cdylib
# through cargo (a no-op when fresh) and refreshes the objs/
# copy nginx loads. The `modules:` prerequisite merges with
# any the generated rules declare.
PAGESPEED_RUST_DIR=`cd $ngx_addon_dir/.. && pwd`

cat << END                                            >> $NGX_MAKEFILE

# pagespeed-rust (cargo-owned binary).
PAGESPEED_RUST_DIR = $PAGESPEED_RUST_DIR
PAGESPEED_RUST_SO = $NGX_OBJS/ngx_pagespeed_rust.so

modules: \$(PAGESPEED_RUST_SO)

\$(PAGESPEED_RUST_SO): FORCE
	cd \$(PAGESPEED_RUST_DIR) && cargo build --release -p ps-nginx
	cp \$(PAGESPEED_RUST_DIR)/target/release/libps_nginx.so \$@

FORCE:

END
