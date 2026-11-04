# License text overrides

Canonical license texts used by `scripts/generate_notices.py` only when a
published crate declares an SPDX license but ships no license file.

Retrieved 2026-11-04 from the SPDX license list data repository
(<https://github.com/spdx/license-list-data>, `main`, `text/`):

- `MIT.txt` — <https://raw.githubusercontent.com/spdx/license-list-data/main/text/MIT.txt>
- `MPL-2.0.txt` — <https://raw.githubusercontent.com/spdx/license-list-data/main/text/MPL-2.0.txt>
- `BSL-1.0.txt` — <https://raw.githubusercontent.com/spdx/license-list-data/main/text/BSL-1.0.txt>

All three files are reproduced byte-for-byte. They carry no package-specific
copyright holder, which is why they are safe to reuse for a package that
embedded no notice of its own. Packages that do ship a license file never use
these overrides; the file from the package is copied instead.

Update procedure: re-fetch the same URLs from a reviewed SPDX
license-list-data commit, record that commit here, and regenerate the bundle.
