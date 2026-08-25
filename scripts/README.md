# Neon3 Scripts

`start-ui-case.cmd` launches a legacy React UI example in three separate CMD
windows:

1. `neon-wgpu-runtime`
2. `neon-ui-runtime`
3. The selected React case

Examples:

```text
scripts\start-ui-case.cmd workbench
scripts\start-ui-case.cmd terrain
scripts\start-ui-case.cmd terrain-generation --projectd
scripts\start-ui-case.cmd ui-platform
```

The launcher requires `packages\neon-ui-react-client`. That package is not
included in this checkout; the script now fails before starting partial
services when it is absent. Use the built-in `component-gallery` command in
the repository README for a standalone windowed smoke test.

The default loopback ports are:

- UI Runtime: `40100`
- WGPU Runtime: `40101`
- Projectd: `40102` when `--projectd` is supplied

The React case receives the UI Runtime port as its first argument. To add a new
case, add its npm script mapping in the `:case_command` section of the CMD file.
