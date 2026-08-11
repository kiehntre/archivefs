# Branding assets

The approved production artwork now lives in
[`assets/branding/`](../../../assets/branding/). That directory is the
canonical source used by the GUI and Linux release packaging. This document
remains here so existing documentation links keep working.

`assets/branding/emuwiz-logo.png` is the canonical, approved EmuWiz logo
master. It was approved before repository inclusion and must not be
redesigned, regenerated, or otherwise altered in place.

The `512`, `256`, `128`, `64`, and `32` PNGs beside the master are
deterministic resized derivatives of that exact master (square, alpha
preserved). They must not be independently regenerated, recomposed, or cropped
- if a new size is ever needed, derive it from `emuwiz-logo.png` the same way.
