# Contributing

Thanks for helping improve boundarycheck. Please open an issue before making a
large behavioral change so the comparison contract can be agreed first.

## Development checks

Rust 1.85 is the minimum supported version. Before opening a pull request, run:

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
python3 scripts/check_markdown_links.py
cargo audit
```

Changes to comparison logic should include a regression test that demonstrates
why no false `PASS` can be produced. Changes to a real-runtime baseline must
explain every non-PASS result and be regenerated with:

```bash
cargo build --release --locked
python3 scripts/compat_matrix.py --write-baselines
```

Do not commit reports, credentials, customer data, runtime logs, virtual
environments, or build output. Security issues belong in GitHub's private
vulnerability reporting flow described in [SECURITY.md](SECURITY.md).
