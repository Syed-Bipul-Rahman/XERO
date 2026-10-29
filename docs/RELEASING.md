# Releasing Xero

## One-time setup

1. **Require CI for pull requests.** Go to *Settings → Branches → Add branch
   ruleset* (or *branch protection rule*) for `main` and enable:
   - *Require a pull request before merging*
   - *Require status checks to pass*, and add the check **`CI passed`**

   `CI passed` succeeds only when the test suite passes on Linux, macOS and
   Windows and the installer checks pass.
2. **Installer site.** Go to *Settings → Pages* and set the source to
   *GitHub Actions*, with the custom domain `download.xerolang.org`. At your DNS
   provider, add a `CNAME` record pointing `download` to
   `syed-bipul-rahman.github.io`. Then enable *Enforce HTTPS*.

## Cutting a release

```sh
git checkout main && git pull
# bump `version` in Cargo.toml, commit, merge
git tag v0.1.0
git push origin v0.1.0
```

Or go to *Actions → Release → Run workflow* and enter the tag.

The `Release` workflow then:

1. builds and tests `xeroc` for Linux (x86-64, ARM64; static musl), macOS
   (ARM64, x86-64) and Windows x86-64;
2. publishes a GitHub release with the archives, `SHA256SUMS`, `install.sh` and
   `install.ps1`;
3. deploys `install/` (the scripts and a landing page) to GitHub Pages at
   `download.xerolang.org`.

The installers always download from `releases/latest`, so a new release
reaches the one-line install commands as soon as it's published.
