# Vendored public suffix list

`public_suffix_list.dat` is the Mozilla Public Suffix List, source of
truth for `domain_registry::minimal_private_suffix` (which hostnames
count as related for proxy-suffix decisions).

Source: `/usr/share/publicsuffix/public_suffix_list.dat` (system copy,
itself pulled from <https://publicsuffix.org/list/public_suffix_list.dat>).
Regenerate by re-copying that file (or a fresh upstream pull); the
registry tests pin multi-level (`co.uk`), provider (`appspot.com`), and
unknown-TLD behavior so a stale or truncated copy fails loudly.
