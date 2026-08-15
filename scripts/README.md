# Neon3 Scripts

Use `start-ui-case.cmd` to launch a UI example with one command. The script opens
three separate CMD windows:

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

The default loopback ports are:

- UI Runtime: `40100`
- WGPU Runtime: `40101`
- Projectd: `40102` when `--projectd` is supplied

The React case receives the UI Runtime port as its first argument. To add a new
case, add its npm script mapping in the `:case_command` section of the CMD file.
