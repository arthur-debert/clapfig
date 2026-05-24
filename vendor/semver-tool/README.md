# semver-tool (vendored)

Vendored from <https://github.com/fsaintjacques/semver-tool> (Apache
2.0). Source file: `src/semver`. Used by `bin/changelog*` for
version validation, parsing, and comparison so we don't reinvent
semver §11.

## Updating

```sh
curl -sSL https://raw.githubusercontent.com/fsaintjacques/semver-tool/master/src/semver \
  -o vendor/semver-tool/semver
chmod +x vendor/semver-tool/semver
```

Then run the BATS suite (issue #205) to confirm nothing in our
callers regressed against the new version.

## License

Apache License 2.0. Full text in `vendor/semver-tool/LICENSE`. Do
not edit the vendored `semver` script — re-vendor from upstream
instead.
