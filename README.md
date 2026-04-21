# asc-sync

`asc-sync` is a small Rust CLI that reconciles App Store Connect provisioning resources from a compact JSON config.

## Install

Install the latest release binary in one step on macOS/Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/iivankin/asc-sync/main/install.sh | bash
```

By default this installs `asc-sync` into `~/.local/bin`.

Install the latest release binary in one step on Windows (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -Command "irm https://raw.githubusercontent.com/iivankin/asc-sync/main/install.ps1 | iex"
```

This installs `asc-sync.exe` into `%USERPROFILE%\.local\bin` and adds that directory to your user `PATH` if needed.

After the crate is published, you can also install it from crates.io:

```bash
cargo install asc-sync
```

Supported resource kinds:

- bundle IDs
- bundle ID capabilities
- devices
- modern Xcode 11+ signing certificates
- provisioning profiles
- existing App Store Connect app records, editable version metadata, review details, and media

Current certificate support is intentionally limited to the modern unified types:

- `development`
- `distribution`
- `developer_id_application`
- `developer_id_installer`

- `developer_id_application` is for signing macOS apps outside the Mac App Store. `asc-sync`
  tracks it internally as `DEVELOPER_ID_APPLICATION_G2`, but `apply` uses a manual CSR flow:
  it generates a CSR, asks you to create the certificate in the Apple Developer portal, then
  downloads the issued certificate from App Store Connect and imports it into the signing bundle.
- `developer_id_installer` is for signing installer packages outside the Mac App Store. It uses
  the same manual CSR flow and is not referenced by provisioning profiles.

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
  "$schema": "https://orbitstorage.dev/schemas/asc-sync.schema-0.1.1.json",
  "_description": "This file is documented by its `$schema`. Start with `ascs --help` for the common workflow.",
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
  },
  "apps": {
    "main": {
      "bundle_id_ref": "main",
      "shared": {
        "primary_locale": "en-US",
        "content_rights_declaration": "does_not_use_third_party_content"
      },
      "platforms": {
        "ios": {
          "version": {
            "version_string": "1.4.0",
            "build_number": "456",
            "release": {
              "type": "manual"
            },
            "localizations": {
              "en-US": "./locale/ios/1.4.0/en-US.json5"
            },
            "review": {
              "contact_first_name": "Ivan",
              "contact_last_name": "Ivanov",
              "contact_email": { "$env": "ASC_REVIEW_CONTACT_EMAIL" },
              "contact_phone": { "$env": "ASC_REVIEW_CONTACT_PHONE" },
              "demo_account_required": false,
              "notes": { "$env": "ASC_REVIEW_NOTES" }
            },
            "media": {
              "en-US": {
                "screenshots": {
                  "iphone67": {
                    "render": {
                      "template": "./screenshots/app-store/*.html",
                      "screens": "./screens/en-US/*.png",
                      "frame": "iPhone 16 Pro - Black Titanium - Portrait"
                    }
                  }
                }
              }
            }
          }
        }
      }
    }
  }
}
```

Version localization files are JSON5 and use the same keys as inline version localization objects:

```json5
{
  description: "Long App Store description",
  keywords: ["sync", "provisioning"],
  support_url: "https://acme.example/support",
  whats_new: "Bug fixes"
}
```

Media stays in the `media` blocks of `asc.json`; localization JSON5 files are for ASC text fields and render template variables.

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

Audit live App Store version keywords before applying metadata changes:

```bash
cargo run -- metadata keywords audit --app 123456789 --version 1.2.3
cargo run -- metadata keywords audit --config asc.json --app 123456789 --version 1.2.3 --blocked-term tracker --blocked-terms-file ./blocked-terms.txt
cargo run -- metadata keywords audit --config asc.json --app 123456789 --version-id 987654321 --strict --output table
```

The audit reports duplicate keyword phrases, repeated phrases across locales, overlap with localized
app name/subtitle text, character-budget usage, underfilled keyword fields, malformed separators,
empty segments, and optional blocked terms. If multiple auth teams are imported, pass `--team-id`
or `--config`.

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

Synchronize App Store Connect metadata from the `apps` block:

```bash
cargo run -- plan --config asc.json
cargo run -- apply --config asc.json
```

`apply` expects the App Store Connect app record to exist. If it is missing, `asc-sync`
prints the required manual action and polls App Store Connect until the record appears,
then continues automatically.

Submit the configured version for review:

```bash
cargo run -- submit-for-review --config asc.json --app main --platform ios
```

`submit-for-review` is intentionally separate from `submit`. The config describes the
desired version metadata and release policy; the command performs the review submission action.

The `apps` block can also describe App Store resource families that live next to the version:

```json5
{
  "apps": {
    "main": {
      "bundle_id_ref": "main",
      "availability": {
        "territories": { "mode": "include", "values": ["USA", "CAN"] }
      },
      "pricing": {
        "base_territory": "USA",
        "replace_future_schedule": true,
        "schedule": [{ "price": "0.99" }]
      },
      "custom_product_pages": {
        "summer": {
          "name": "Summer 2026",
          "deep_link": "myapp://summer",
          "visible": true,
          "localizations": {
            "en-US": {
              "promotional_text": "Try the summer flow",
              "headline": "Summer-only onboarding"
            }
          },
          "media": {
            "en-US": {
              "screenshots": {
                "iphone67": {
                  "render": {
                    "template": "./screenshots/cpp/summer/*.html",
                    "screens": "./screens/en-US/*.png",
                    "frame": "iPhone 16 Pro - Black Titanium - Portrait",
                    "output_dir": "./media/cpp/summer/en-US/iphone67"
                  }
                }
              }
            }
          }
        }
      },
      "in_app_purchases": {
        "coins_100": {
          "product_id": "com.example.app.coins100",
          "type": "consumable",
          "reference_name": "100 Coins",
          "localizations": {
            "en-US": { "name": "100 Coins", "description": "A coin pack." }
          },
          "review": { "screenshot": "./review/coins100.png" }
        }
      },
      "subscription_groups": {
        "premium": {
          "reference_name": "Premium",
          "subscriptions": {
            "monthly": {
              "product_id": "com.example.app.premium.monthly",
              "reference_name": "Premium Monthly",
              "period": "one_month",
              "group_level": 1,
              "localizations": {
                "en-US": { "name": "Monthly", "description": "Full access." }
              }
            }
          }
        }
      },
      "app_events": {
        "launch": {
          "reference_name": "Launch Challenge",
          "badge": "challenge",
          "territory_schedules": [{
            "territories": ["USA"],
            "publish_start": "2026-06-01T10:00:00Z",
            "event_start": "2026-06-02T10:00:00Z",
            "event_end": "2026-06-10T10:00:00Z"
          }],
          "localizations": {
            "en-US": {
              "name": "Launch Challenge",
              "short_description": "Try the new mode.",
              "long_description": "Complete tasks and unlock rewards."
            }
          },
          "media": {
            "en-US": { "card_image": "./events/launch/card.png" }
          }
        }
      },
      "privacy": {
        "uses_tracking": false,
        "data_types": [{
          "type": "precise_location",
          "linked_to_user": true,
          "tracking": false,
          "purposes": ["app_functionality"]
        }]
      },
      "platforms": {
        "ios": { "version": { "version_string": "1.2.3" } }
      }
    }
  }
}
```

Localizations in custom product pages, in-app purchases, subscription groups,
subscriptions, and app events can be inline objects or JSON5 file paths. Custom product
page screenshot render templates use the custom product page localization strings for
that locale, including extra JSON5 keys such as `headline`. Pricing can use
`price_point_id` directly, or `price`, which is resolved through the App Store Connect
price point list for the configured base territory.

Example custom product page localization file:

```json5
{
  promotional_text: "Try the summer flow",
  headline: "Summer-only onboarding"
}
```

`apply` creates and updates safe metadata for custom product pages, IAPs, subscriptions,
subscription groups, app events, localizations, and review/media assets. App privacy is a
typed checklist because App Store privacy answers are not safely writable through this
sync path. Existing commerce availability and IAP/subscription price changes are reported
as manual/review-sensitive follow-ups instead of being silently mutated.

Validate App Store media locally:

```bash
cargo run -- media validate --config asc.json
```

Render App Store screenshots from plain HTML:

```bash
cargo run -- media preview --input './screenshots/app-store/*.html' --size iphone67 --open
cargo run -- media render --input './screenshots/app-store/*.html' --size iphone67 --output-dir './media/en-US/iphone'
```

`media render` uses headless Chrome/Chromium through the Chrome DevTools Protocol. It waits
for document load, stylesheet readiness, fonts, image loading/decoding, and one animation
frame before capturing. It does not read YAML and does not run Python. Pass
`--chrome /path/to/chrome` or set `CHROME_BIN` if Chrome is not in a standard location.
macOS discovery checks Chrome Stable, Beta, Dev, Canary, Chrome for Testing, and Chromium
in `/Applications` and `~/Applications`.
`--input` accepts one or more HTML files, directories, or glob patterns; directories and
globs are sorted lexicographically. Output file names use the HTML file stem, so
`01-home.html` renders to `01-home.png`. PNG output is flattened onto a white background.

The same renderer can be used directly from `asc.json` for version media:

```json5
"localizations": {
  "en-US": "./locale/ios/1.4.0/en-US.json5"
},
"media": {
  "en-US": {
    "screenshots": {
      "iphone67": {
        "render": {
          "template": "./screenshots/app-store/*.html",
          "screens": "./screens/en-US/*.png",
          "frame": "iPhone 16 Pro - Black Titanium - Portrait",
          "output_dir": "./media/en-US/iphone67"
        }
      }
    }
  }
}
```

Config render output is temporary by default. Set `output_dir` to persist the rendered
PNGs; `validate`/`apply` still render before validation/upload, validate the resulting
files as normal App Store screenshots, and upload them in resolved template order. The
render strings come from `version.localizations[locale]`, so extra JSON5 keys such as
`hero.title` are available as `{{hero.title}}`.

Bundled device frames can wrap each HTML template:

```bash
cargo run -- media preview \
  --input './screenshots/app-store/*.html' \
  --screen './screens/app.png' \
  --size iphone67 \
  --frame 'iPhone 16 Pro - Black Titanium - Portrait' \
  --locale en-US \
  --strings './locale/en-US.json5' \
  --open

cargo run -- media render \
  --input './screenshots/app-store/*.html' \
  --screen './screens/app.png' \
  --size iphone67 \
  --frame 'iPhone 16 Pro - Black Titanium - Portrait' \
  --locale en-US \
  --strings './locale/en-US.json5' \
  --output-dir './media/en-US/iphone'
```

Frame names are PNG stems from the device frame manifest on `https://orbitstorage.dev`.
When a frame is needed and no local frame directory is configured, `asc-sync` downloads
`manifest.json`, verifies MD5/size for the required frame assets, and caches them in
`~/.asc-sync/device-frames`. It downloads only the requested frame PNG plus shared
metadata files.

For local development or private frames, use `ASC_SYNC_FRAMES_DIR` or `--frame-dir` to
point at a directory with device frame PNG files plus `Frames.json`. Set
`ASC_SYNC_DEVICE_FRAMES_URL` to override the remote manifest base URL.

Upload the local frame directory to Orbit Storage:

```bash
scripts/upload-device-frames.sh
```

The upload script reads `assets/device-frames`, uploads files to
`https://orbitstorage.dev/assets/device-frames`, and writes a sibling `manifest.json`
with file sizes and MD5 hashes. It uses the same Cloudflare R2 environment variables as
the schema upload workflow: `CLOUDFLARE_R2_ACCESS_KEY_ID`,
`CLOUDFLARE_R2_SECRET_ACCESS_KEY`, `CLOUDFLARE_R2_BUCKET`, and
`CLOUDFLARE_R2_ENDPOINT`.

`--screen` accepts one image for all
HTML templates, or a file/directory/glob with the same number of images as templates.
When `--frame` is set, the HTML template must contain `<asc-device-frame></asc-device-frame>`
where the framed screen should appear. The screen is precomposed into the frame as a PNG,
then that framed asset is scaled by normal CSS layout. Screen placement uses
`Frames.json` geometry. If `Frames.json` does not contain the frame, `asc-sync` falls back
to detecting the inner transparent
screen area from the frame PNG alpha channel.

`--strings` accepts JSON or JSON5. String placeholders are HTML-escaped and can use dot
paths. Built-in placeholders are `{{locale}}`, `{{id}}`, and `{{asc_id}}`.

Use `--size` for App Store named sizes such as `iphone67`, `iphone65`, `ipad13`, `mac`,
`apple_tv`, or `vision_pro`. Use `--viewport 1320x2868` for a custom exact viewport.
HTML templates should fill the browser viewport and place the frame explicitly:

```html
<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <style>
    html, body { margin: 0; width: 100%; height: 100%; }
    body { background: linear-gradient(#f8f1dc, #d5e5ff); }
    .shot { width: 100vw; height: 100vh; display: grid; grid-template-columns: 1fr 48%; align-items: center; padding: 8vw; box-sizing: border-box; }
    asc-device-frame { width: 100%; }
  </style>
</head>
<body>
  <main class="shot">
    <h1>{{hero.title}}</h1>
    <asc-device-frame fit="cover"></asc-device-frame>
  </main>
</body>
</html>
```

Media validation also runs before `apply`. Screenshots are checked for count, extension,
and Apple-accepted dimensions for the configured display type. App previews are checked
for count, extension, file size, resolution, duration, frame rate, progressive video, and
H.264/ProRes 422 HQ codec. Custom product page media uses the same screenshot/preview
rules. IAP and subscription review screenshots are checked as image files. App event
images are checked as 1920x1080 or 3840x2160 assets, and app event videos are checked as
16:9 progressive H.264/ProRes assets up to 60fps. Preview and event video validation
requires `ffprobe` from FFmpeg.

If you change `team_id` in `asc.json`, the next mutating config-based command that opens
`signing.ascbundle` hard-resets it to an empty state for the new team. This is a destructive
cutover, not a migration. Read-only commands fail instead of rewriting the bundle.

On the first `apply`, if `signing.ascbundle` does not exist yet, `asc-sync`:

- generates separate strong passwords for `developer` and `release`
- prints them once in the terminal
- stores them in `~/.asc-sync/bundle-passwords/`
- creates `signing.ascbundle` next to `asc.json`

When `apply` needs to create a `developer_id_application` or `developer_id_installer` certificate,
it pauses in an interactive terminal, prints the generated CSR path, asks you to create the
matching Developer ID certificate in Certificates, Identifiers & Profiles, and then polls
App Store Connect until the new certificate appears so it can download and import it automatically.

For `developer_id_application`, `apply` also expects Apple to expose the new certificate back
through the API so `mac_app_direct` provisioning profiles can reference it.

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
  - distribution, Developer ID Application, and Developer ID Installer certificates
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

App Store Connect may report `IN_APP_PURCHASE` as an enabled bundle capability
even when the config omits it. `asc-sync` treats that capability as
App-Store-Connect-owned for disables, because ASC rejects deletion for universal
App IDs.

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

Logical keys for `bundle_ids`, `devices`, `certs`, `profiles`, and `apps` are stable IDs, not display names.
They must use only ASCII letters, digits, `.`, `-`, and `_`.

The JSON Schema is checked into [schema/asc-sync.schema.json](/Users/ilyai/Developer/personal/asc-sync/schema/asc-sync.schema.json).
