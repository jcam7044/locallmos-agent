# Agent icons

Tauri needs bundle icons referenced in `tauri.conf.json`. Generate them from a
single source image (1024×1024 PNG recommended):

```bash
pnpm --filter @locallmos/agent tauri icon path/to/logo.png
```

This produces `32x32.png`, `128x128.png`, `icon.icns`, `icon.ico`, and the
platform icon sets. Until then, `tauri build` (and `tauri dev` on some
platforms) will fail with a missing-icon error.
