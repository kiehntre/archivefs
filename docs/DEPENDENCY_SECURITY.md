# Dependency security

This note records the dependency-advisory remediation performed for the
EmuWiz v0.7 alpha candidate on 2026-07-29. It covers the resolved Cargo
graph; it does not claim that all future dependency versions are safe.

## Findings and exposure

| Advisory | Before | Exposure in EmuWiz | Resolution |
| --- | --- | --- | --- |
| RUSTSEC-2026-0195 | `quick-xml 0.39.4` | Reached at build time through the `wayland-scanner` procedural macro. quick-xml is also a runtime dependency of `archivefs-core`, where it parses Logiqx DAT/catalogue XML. | Updated to `quick-xml 0.41.0` through `wayland-scanner 0.31.11`. |
| RUSTSEC-2026-0194 | `quick-xml 0.39.4` | Reached at build time through the same Wayland protocol-generation path. quick-xml is also a runtime dependency of `archivefs-core`, where it parses Logiqx DAT/catalogue XML. | Updated to `quick-xml 0.41.0` through `wayland-scanner 0.31.11`. |
| RUSTSEC-2026-0192 | `ttf-parser 0.25.1` | Runtime GUI font parsing and rendering through `owned_ttf_parser`, `ab_glyph`, `epaint`, and `egui`. The finding is an unmaintained-crate warning rather than a known vulnerability. | Removed by updating the coordinated `eframe`/`egui` family from 0.32.3 to 0.34.3. The replacement font path uses `font-types 0.11.3`, `read-fonts 0.37.0`, and `skrifa 0.40.0`. |

Before remediation, the relevant dependency paths were:

```text
quick-xml 0.39.4
└── wayland-scanner 0.31.10 (proc-macro)
    └── Wayland client/protocol crates
        └── winit / smithay-clipboard / rfd
            └── eframe
                └── archivefs-gui

ttf-parser 0.25.1
└── owned_ttf_parser 0.25.1
    └── ab_glyph 0.2.32
        └── epaint / egui / eframe
            └── archivefs-gui
```

After remediation, the paths are:

```text
quick-xml 0.41.0
└── wayland-scanner 0.31.11 (proc-macro)
    └── the existing Wayland client/protocol chain

font-types 0.11.3 / read-fonts 0.37.0 / skrifa 0.40.0
└── epaint / egui / eframe 0.34.3
    └── archivefs-gui
```

`cargo tree -i ttf-parser` no longer matches any resolved package.

## Chosen remediation

The quick-xml findings were fixed with a compatible Wayland scanner patch-line
lockfile update. The font warning could not be removed by a lockfile-only
update: the current `ab_glyph 0.2` line constrains `owned_ttf_parser` to the
unmaintained 0.25 line. EmuWiz therefore uses the smallest coordinated GUI
dependency update that removes that chain without changing application
features or redesigning the interface.

The GUI remains configured with `default-features = false` and the explicit
`default_fonts`, `glow`, `wayland`, and `x11` features. Clipboard support and
the `rfd` desktop file-dialog path remain resolved. Disabling Wayland, X11,
clipboard, or file dialogs was rejected because those are supported runtime
capabilities, not unused features.

egui 0.34 retains deprecated compatibility entry points used by the current
large GUI module. This security-focused update keeps those entry points behind
a documented crate-level deprecation allowance rather than combining the
dependency repair with a GUI layout migration. The compiler still enforces all
other warnings through Clippy's `-D warnings` gate. Removing the compatibility
allowance is follow-up migration work, not an unresolved security advisory.

## Dependency-graph impact

The resolved lockfile grows from 393 to 402 audited packages. The obsolete
`ab_glyph`, `ab_glyph_rasterizer`, `owned_ttf_parser`, and `ttf-parser` packages
are removed. The maintained font stack and its supporting packages are added.
An additional `foldhash`/`hashbrown` version pair is present in the resolved
graph; the pre-existing duplicate families for `calloop`, `getrandom`,
`rustix`, `smithay-client-toolkit`, and `thiserror` remain. No application
dependency was added solely to suppress an advisory.

The lockfile contains optional backend packages published with the updated GUI
family, but EmuWiz still selects the Glow renderer. `cargo tree -p
archivefs-gui -e normal` confirms that WGPU and Naga are not in the active
normal dependency graph.

## Verification

The required verification commands are:

```sh
cargo tree --workspace -i quick-xml -e features
cargo tree --workspace -i ttf-parser -e features
cargo audit
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release --locked
```

At the time of remediation, `cargo audit` loaded 1,173 RustSec advisories,
scanned 402 resolved dependencies, and returned success with no vulnerabilities,
warnings, or informational advisories. No advisory ignore was added.

Release-artifact verification and byte-for-byte reproducibility remain required
under [RELEASE_ENGINEERING.md](RELEASE_ENGINEERING.md). Desktop QA must cover
X11 startup, Wayland startup when available, Gamer View, search, platform
cards, local PNG artwork, file dialogs, clipboard copy/paste, Advanced View,
PCSX2 recognition, and rendering stability using the exact release binary.

### Manual QA result

The exact verified release artifact was extracted under `/tmp` and launched
through the user's Sunshine X display. The user confirmed that X11 startup,
Gamer View, search filtering, platform cards, built-in artwork fallback, the
file dialog, clipboard copy/paste, Advanced View and return navigation, PCSX2
profile recognition, and general rendering/stability all passed.

The user's custom-artwork preference was empty and no local `gamecube.png` was
available, so a valid custom PNG could not be rendered during this manual run.
The safe built-in fallback was confirmed manually, while valid/malformed/cache
PNG behavior remains covered by the focused GUI tests. Native Wayland startup
was unavailable in the Sunshine session, which exposed only X display `:0`.
Neither limitation is reported as a manual pass.

## Remaining limitations

- The egui compatibility entry points are deprecated and should be migrated in
  a dedicated GUI change after their layout semantics have been reviewed.
- Passing Cargo and RustSec checks does not replace desktop testing on each
  supported display server.
- RustSec results describe the advisory database and resolved graph at the time
  of the run; CI must repeat the audit for later commits.
