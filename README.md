# asc-sync

`asc-sync` is a small Rust CLI that reconciles App Store Connect provisioning resources from a compact JSON config.

Supported resource kinds:

- bundle IDs
- bundle ID capabilities
- devices
- modern Xcode 11+ signing certificates
- provisioning profiles

Current certificate support is intentionally limited to the modern unified types:

- `development`
- `distribution`
- `developer_id_application`

## Auth Import

Import App Store Connect API auth once per team:

```bash
cargo run -- auth import
```

The command asks for:

- `teamId`
- `issuerId`
- `keyId`
- the local path to your `AuthKey_*.p8`

If `asccli.sh` is already installed and configured, `auth import` first offers to reuse an existing
`asc` profile from `~/.asc/config.json` / its keychain-backed storage instead of asking you to type
the key details again.

`asc-sync` stores the imported auth in `~/.asc-sync/auth/<team_id>.json`.

For CI or other non-interactive environments, you can skip `auth import` and set environment variables instead:

```bash
export ASC_ISSUER_ID="00000000-0000-0000-0000-000000000000"
export ASC_KEY_ID="ABC123DEFG"
export ASC_PRIVATE_KEY_PATH="./keys/AuthKey_ABC123DEFG.p8"
```

You can also pass the private key directly:

```bash
export ASC_PRIVATE_KEY="$(cat ./keys/AuthKey_ABC123DEFG.p8)"
```

## Config File

Create a new manifest:

```bash
cargo run -- init
```

`init` writes `asc.json` with a version-pinned published schema URL on `https://orbitstorage.dev/schemas/`.
It does not ask for `team_id`; it selects from the imported auth entries already stored in `~/.asc-sync`.

Pass the desired state with `--config asc.json`.

```json
{
  "$schema": "https://orbitstorage.dev/schemas/asc-sync.schema-0.1.0.json",
  "team_id": "TEAMID1234",
  "bundle_ids": {
    "main": {
      "bundle_id": "com.acme.app",
      "name": "Acme App",
      "platform": "ios",
      "capabilities": [
        "push_notifications",
        "associated_domains",
        {
          "icloud": {
            "version": "xcode_6"
          }
        },
        {
          "data_protection": {
            "level": "protected_until_first_user_auth"
          }
        },
        {
          "apple_id_auth": {
            "app_consent": "primary_app_consent"
          }
        }
      ]
    },
    "desktop": {
      "bundle_id": "com.acme.desktop",
      "name": "Acme Desktop",
      "platform": "mac_os"
    }
  },
  "devices": {
    "ivan-iphone": {
      "family": "ios",
      "udid": "00008110-001234567890801E",
      "name": "Ivan iPhone 15 Pro"
    },
    "build-mac": {
      "family": "macos",
      "udid": "ABCD1234EFGH5678IJKL9012MNOP3456",
      "name": "Build Mac"
    }
  },
  "certs": {
    "dev": {
      "type": "development",
      "name": "Acme Apple Development"
    },
    "app-store": {
      "type": "distribution",
      "name": "Acme Apple Distribution"
    },
    "direct": {
      "type": "developer_id_application",
      "name": "Acme Developer ID Application"
    }
  },
  "profiles": {
    "ios-development": {
      "name": "Acme iOS Development",
      "type": "ios_app_development",
      "bundle_id": "main",
      "certs": ["dev"],
      "devices": ["ivan-iphone"]
    },
    "ios-app-store": {
      "name": "Acme iOS App Store",
      "type": "ios_app_store",
      "bundle_id": "main",
      "certs": ["app-store"]
    },
    "mac-direct": {
      "name": "Acme Mac Direct",
      "type": "mac_app_direct",
      "bundle_id": "desktop",
      "certs": ["direct"]
    }
  }
}
```

## Commands

Validate config only:

```bash
cargo run -- validate --config asc.json
```

If `signing.ascbundle` exists, `validate` also:

- verifies the bundle belongs to the same `team_id`
- when App Store Connect auth is available, verifies managed bundle IDs still exist in ASC
- when App Store Connect auth is available, verifies managed devices still exist and remain `ENABLED` in ASC
- checks managed certificates in the bundle are not expired
- checks managed provisioning profiles in the bundle are not expired
- when App Store Connect auth is available, verifies managed certificates still exist and are active in ASC
- when App Store Connect auth is available, verifies managed provisioning profiles still exist and remain `ACTIVE` in ASC

It always needs the relevant bundle passwords to open encrypted sections. Live ASC checks are best-effort and run only when auth is available.

If you prefer local editor validation, the repository schema source is still at:

- `schema/asc-sync.schema.json`

Show the planned changes:

```bash
cargo run -- plan --config asc.json
```

Apply the desired state:

```bash
cargo run -- apply --config asc.json
```

Submit a macOS Developer ID artifact for notarization and staple it on success:

```bash
cargo run -- notarize --config asc.json --file ./MyApp.pkg
cargo run -- notarize --config asc.json --file ./MyApp.app
```

`notarize` uses the imported App Store Connect API key, submits through `xcrun notarytool`, waits
for completion, and staples the original `.app`, `.pkg`, or `.dmg` when stapling is applicable.

Submit an App Store build to App Store Connect:

```bash
cargo run -- submit --config asc.json --file ./MyApp.ipa
cargo run -- submit --config asc.json --file ./MyMacApp.pkg --bundle-id desktop
```

`submit` uses `xcrun altool --upload-package`. It requires that an App Store Connect
app record already exists for the chosen `bundle_id`; create the app record first in
App Store Connect before submitting.

If `asc.json` contains more than one `bundle_ids` entry, `submit` requires `--bundle-id <logical-id>`
to choose which app record to target.

If you change `team_id` in `asc.json`, the next mutating config-based command that opens
`signing.ascbundle` hard-resets it to an empty state for the new team. This is a destructive
cutover, not a migration. Read-only commands fail instead of rewriting the bundle.

On the first `apply`, if `signing.ascbundle` does not exist yet, `asc-sync`:

- generates separate strong passwords for `developer` and `release`
- prints them once in the terminal
- stores them in `~/.asc-sync/bundle-passwords/`
- creates `signing.ascbundle` next to `asc.json`

Import signing material on a new machine or in CI:

```bash
export ASC_DEVELOPER_BUNDLE_PASSWORD='developer-password'
export ASC_RELEASE_BUNDLE_PASSWORD='release-password'
cargo run -- signing import --config asc.json
```

Print the recommended manual-signing settings for each managed provisioning profile:

```bash
cargo run -- signing print-build-settings --config asc.json
```

Create a remote iPhone/iPad registration token against the shared device server:

```bash
cargo run -- device add --config asc.json --name "Ivan iPhone 16" --apply
```

By default `device add` uses `https://asc.orbitstorage.dev`. Set `ASC_DEVICE_SERVER_URL` only if you need to override it for local testing.

`device add` does three things:

- creates a one-time registration token on the shared server
- prints the registration link and a terminal QR code
- waits for the device to report its UDID, then writes the device into `asc.json`
- only registers the device in App Store Connect when `--apply` is present
- with `--apply`, also writes the managed device into the shared `state.json` inside `signing.ascbundle`

Register a local device directly:

```bash
cargo run -- device add-local --config asc.json --current-mac --apply
cargo run -- device add-local --config asc.json --apply
cargo run -- device add-local --config asc.json --family ios --udid 00008110-001234567890801E --name "QA iPhone" --apply
```

## Device Server

```bash
export ASC_DEVICE_SERVER_PUBLIC_URL=https://asc.orbitstorage.dev
```

The server serves an unsigned `.mobileconfig` and keeps registration tokens and completion results only in memory, so a container restart invalidates in-flight registration links.

Docker example:

```bash
docker build -t asc-sync-device-server .

docker run --rm -p 3000:3000 \
  -e ASC_DEVICE_SERVER_PUBLIC_URL="https://asc.orbitstorage.dev" \
  asc-sync-device-server
```

Revoke managed certificates and their profiles for one or both scopes:

```bash
cargo run -- revoke dev --config asc.json
cargo run -- revoke release --config asc.json
cargo run -- revoke all --config asc.json
```

Resolve a git conflict for `signing.ascbundle` with a three-way merge:

```bash
git show :1:signing.ascbundle > /tmp/base.ascbundle
git show :2:signing.ascbundle > /tmp/ours.ascbundle
git show :3:signing.ascbundle > /tmp/theirs.ascbundle

cargo run -- signing merge \
  --config asc.json \
  --base /tmp/base.ascbundle \
  --ours /tmp/ours.ascbundle \
  --theirs /tmp/theirs.ascbundle
```

If both sides changed the same shared state entry or the same encrypted scope payload, `signing merge`
asks you to choose `base`, `ours`, or `theirs`. In non-interactive mode it fails and tells you to
rerun the command in a terminal.

## Signing Bundle

The canonical shared artifact is one file:

- `signing.ascbundle` next to `asc.json`

Internally it contains:

- one plain shared `state.json` at the top level
- two independently encrypted signing sections

- a `developer` section for:
  - development certificates
  - development / adhoc profiles
- a `release` section for:
  - distribution and Developer ID certificates
  - App Store / direct distribution profiles

Recommended workflow:

- local machines run `apply` and are the normal writers of `signing.ascbundle`
- CI runs `signing import` and uses the imported signing material read-only
- the bundle is the only persistent backend; there is no `.asc-sync` cache on disk
- shared ownership state lives in plain `state.json` inside the bundle; only signing artifacts are encrypted

Password handling:

- `developer` and `release` always use different passwords
- App Store Connect auth is resolved by `team_id`
- device registration uses `https://asc.orbitstorage.dev` by default; `ASC_DEVICE_SERVER_URL` is only an override
- on the first `apply`, `asc-sync` generates both passwords automatically, prints them once, and stores them in `~/.asc-sync/bundle-passwords/`
- local machines resolve ASC auth from `~/.asc-sync/auth/<team_id>.json`
- if no imported auth exists for that `team_id`, `asc-sync` falls back to `ASC_ISSUER_ID`, `ASC_KEY_ID`, and `ASC_PRIVATE_KEY` or `ASC_PRIVATE_KEY_PATH`
- `device add` and `device add-local` both update `asc.json` before they touch ASC, so the next `apply` does not prune the device back out
- later `plan`, `apply`, `signing import`, and `revoke` try passwords in this order:
  - `ASC_DEVELOPER_BUNDLE_PASSWORD` / `ASC_RELEASE_BUNDLE_PASSWORD`
  - cached password in `~/.asc-sync/bundle-passwords/`
  - interactive prompt, where an empty answer skips that scope
- if you only know one password, only that scope is unlocked and processed
- `signing import` imports unlocked `.p12` bundles into `~/Library/Keychains/login.keychain-db` and also installs unlocked provisioning profiles into `~/Library/MobileDevice/Provisioning Profiles/<uuid>.mobileprovision`
- `signing print-build-settings` prints `DEVELOPMENT_TEAM`, `PROVISIONING_PROFILE_SPECIFIER`, `PROVISIONING_PROFILE`, and the recommended `CODE_SIGN_IDENTITY` for each unlocked managed profile
- `apply` also installs the provisioning profiles it just reconciled into `~/Library/MobileDevice/Provisioning Profiles/<uuid>.mobileprovision`

Runtime state, certificate blobs, and provisioning profiles stay in memory during each command. The only temporary file that still appears on disk is a short-lived `.p12` created just before `security import`.

## Capability DSL

Simple capabilities stay as strings:

```json
["push_notifications", "associated_domains", "app_groups"]
```

Capabilities with settings use compact object forms:

```json
[
  { "icloud": { "version": "xcode_6" } },
  { "data_protection": { "level": "complete_protection" } },
  { "apple_id_auth": { "app_consent": "primary_app_consent" } }
]
```

Profile type values:

- `ios_app_development`
- `ios_app_store`
- `ios_app_adhoc`
- `ios_app_inhouse`
- `tvos_app_development`
- `tvos_app_store`
- `tvos_app_adhoc`
- `tvos_app_inhouse`
- `mac_app_development`
- `mac_app_store`
- `mac_app_direct`
- `mac_catalyst_app_development`
- `mac_catalyst_app_store`
- `mac_catalyst_app_direct`

Logical keys for `bundle_ids`, `devices`, `certs`, and `profiles` are stable IDs, not display names.
They must use only ASCII letters, digits, `.`, `-`, and `_`.

The JSON Schema is checked into [schema/asc-sync.schema.json](/Users/ilyai/Developer/personal/asc-sync/schema/asc-sync.schema.json).
