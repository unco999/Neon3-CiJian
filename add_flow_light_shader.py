path = r'D:\Neon3案例\node\src\cases\music-player\shaders.ts'
with open(path, 'r', encoding='utf-8') as f:
    content = f.read()

flow_light_shader = '''
// pulse-flow-light v1 - dynamic lime light rays rendered into the
// behind_glass composition layer. The system GaussianBlur then softens
// these rays, so they bleed through the glass as ambient volumetric light
// rather than a hard painted stripe. Transparent everywhere except the
// active ray bands and corner glints.
const pulseFlowLight = `
fn hash2(p: vec2<f32>) -> vec2<f32> {
  return vec2<f32>(
    fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453),
    fract(sin(dot(p, vec2<f32>(269.5, 183.3))) * 23421.6312)
  );
}
fn noise2(p: vec2<f32>) -> f32 {
  let cell = floor(p);
  let f = fract(p);
  let u = f * f * (3.0 - 2.0 * f);
  return mix(
    mix(hash2(cell).x, hash2(cell + vec2<f32>(1.0, 0.0)).x, u.x),
    mix(hash2(cell + vec2<f32>(0.0, 1.0)).x, hash2(cell + vec2<f32>(1.0, 1.0)).x, u.x),
    u.y
  );
}
fn fbm2(p: vec2<f32>) -> f32 {
  var value = 0.0;
  var amp = 0.5;
  var q = p;
  for (var i = 0; i < 4; i = i + 1) {
    value = value + noise2(q) * amp;
    q = q * 2.03 + vec2<f32>(13.7, 7.1);
    amp = amp * 0.5;
  }
  return value;
}
fn material(input: MaterialInput) -> vec4<f32> {
  let t = input.time_seconds;
  let p = input.local_position;
  let px = vec2<f32>(p.x * input.bounds.z, p.y * input.bounds.w);

  // Primary diagonal ray band — slow drift, wide soft core.
  let diag_a = p.x * 0.72 - p.y * 1.25;
  let ray_a = exp(-abs(fract(diag_a * 0.85 + t * 0.030) - 0.42) * 14.0);

  // Secondary narrower ray — faster, opposite diagonal.
  let diag_b = p.x * 1.05 + p.y * 0.62;
  let ray_b = exp(-abs(fract(diag_b * 1.2 - t * 0.055) - 0.58) * 22.0) * 0.7;

  // Thin highlight streak that pulses.
  let diag_c = p.x * 0.9 - p.y * 0.9;
  let streak = pow(0.5 + 0.5 * sin(diag_c * 22.0 - t * 0.8), 48.0) * 0.5;

  // Volumetric noise modulates the rays so they don't look like flat lines.
  let vol = fbm2(px * 0.012 + vec2<f32>(t * 0.020, -t * 0.015));
  let ray_mod = 0.55 + 0.65 * vol;

  // Corner glints — bright spots near the cut corners that fade inward.
  let corner_tl = exp(-(p.x * p.x + p.y * p.y) * 48.0) * 0.8;
  let corner_br = exp(-((1.0 - p.x) * (1.0 - p.x) + (1.0 - p.y) * (1.0 - p.y)) * 48.0) * 0.6;
  let corner_tr = exp(-((1.0 - p.x) * (1.0 - p.x) + p.y * p.y) * 64.0) * 0.4;

  // Subtle vertical gradient — brighter near top third.
  let vert = exp(-pow((p.y - 0.28) * 2.4, 2.0)) * 0.25;

  let rays = (ray_a + ray_b + streak) * ray_mod;
  let glints = corner_tl + corner_br + corner_tr;
  let intensity = rays * 0.85 + glints * 0.5 + vert;

  // Lime-yellow core with a hint of warm white in the brightest spots.
  let lime_core = vec3<f32>(0.62, 1.0, 0.12);
  let warm_hot = vec3<f32>(0.95, 1.0, 0.72);
  let color = mix(lime_core, warm_hot, smoothstep(0.5, 1.0, intensity));

  // Keep alpha low — this is ambient light, not a solid panel. The blur
  // will spread it further. Max ~0.35 so it never washes out the UI.
  let alpha = clamp(intensity * 0.32, 0.0, 0.35);

  return vec4<f32>(color * alpha, alpha);
}
`;

'''

# Insert before pulse-neon-edge
marker = '// pulse-neon-edge v3'
if marker in content:
    content = content.replace(marker, flow_light_shader + marker, 1)
    print("OK: inserted flow light shader")
else:
    print("MISS: marker not found")

# Add to packageFor list
old_packages = '''  return [
    packageFor("pulse-glass", 6, pulseGlass),
    packageFor("pulse-neon-edge", 3, pulseNeonEdge),
  ];'''
new_packages = '''  return [
    packageFor("pulse-glass", 6, pulseGlass),
    packageFor("pulse-flow-light", 1, pulseFlowLight),
    packageFor("pulse-neon-edge", 3, pulseNeonEdge),
  ];'''
if old_packages in content:
    content = content.replace(old_packages, new_packages, 1)
    print("OK: added to packages")
else:
    print("MISS: packages marker not found")

with open(path, 'w', encoding='utf-8') as f:
    f.write(content)
print("shaders.ts updated")
