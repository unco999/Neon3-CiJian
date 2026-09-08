//! Windows composition host for the windowed runtime.
//!
//! The window has one presentation path: DXGI creates a swapchain for a
//! DirectComposition surface handle, and WinRT Composition consumes that same
//! surface in the single DesktopWindowTarget tree.

use windows::core::{implement, Interface, PCWSTR};
use windows::Graphics::IGeometrySource2D;
use windows::Foundation::{IPropertyValue, PropertyValue};
use windows::Graphics::Effects::{
    IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectSource_Impl, IGraphicsEffect_Impl,
};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    Compositor, CompositionEffectBrush, CompositionEffectSourceParameter, CompositionPath,
    CompositionSurfaceBrush, ContainerVisual, SpriteVisual,
};
use windows::UI::Color;
use windows::Win32::Foundation::{GENERIC_ALL, HANDLE, HWND};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, D2D1_FACTORY_TYPE_SINGLE_THREADED,
};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED, D2D1_FILL_MODE_WINDING,
};
use windows::Win32::Graphics::DirectComposition::DCompositionCreateSurfaceHandle;
use windows::Win32::System::WinRT::Composition::{
    ICompositorDesktopInterop, ICompositorInterop,
};
use windows::Win32::System::WinRT::Graphics::Direct2D::{
    GRAPHICS_EFFECT_PROPERTY_MAPPING, GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT,
    IGeometrySource2DInterop, IGeometrySource2DInterop_Impl, IGraphicsEffectD2D1Interop,
    IGraphicsEffectD2D1Interop_Impl,
};
use windows_numerics::{Vector2, Vector3};

const CLSID_D2D1_GAUSSIAN_BLUR: windows::core::GUID = windows::core::GUID::from_values(
    0x1feb6d69, 0x2fe6, 0x4ac9, [0x8c, 0x58, 0x1d, 0x7f, 0x93, 0xe7, 0xa6, 0xa5],
);

fn invalid_param() -> windows::core::Error {
    windows::core::Error::from_hresult(windows::core::HRESULT(0x80070057u32 as i32))
}

fn prop_str(name: &PCWSTR) -> String {
    unsafe { name.as_wide() }
        .iter()
        .map(|&c| char::from_u32(c as u32).unwrap_or('?'))
        .collect()
}

#[implement(IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectD2D1Interop)]
struct GaussianBlurEffect {
    name: std::cell::RefCell<windows::core::HSTRING>,
    blur_amount: f32,
    optimization: u32,
    border_mode: u32,
    source: IGraphicsEffectSource,
}

impl IGraphicsEffectSource_Impl for GaussianBlurEffect_Impl {}
impl IGraphicsEffect_Impl for GaussianBlurEffect_Impl {
    fn Name(&self) -> windows::core::Result<windows::core::HSTRING> {
        Ok(self.name.borrow().clone())
    }
    fn SetName(&self, name: &windows::core::HSTRING) -> windows::core::Result<()> {
        *self.name.borrow_mut() = name.clone();
        Ok(())
    }
}
impl IGraphicsEffectD2D1Interop_Impl for GaussianBlurEffect_Impl {
    fn GetEffectId(&self) -> windows::core::Result<windows::core::GUID> {
        Ok(CLSID_D2D1_GAUSSIAN_BLUR)
    }
    fn GetNamedPropertyMapping(
        &self,
        name: &PCWSTR,
        index: *mut u32,
        mapping: *mut GRAPHICS_EFFECT_PROPERTY_MAPPING,
    ) -> windows::core::Result<()> {
        let index_value = match prop_str(name).as_str() {
            "BlurAmount" => 0,
            "Optimization" => 1,
            "BorderMode" => 2,
            _ => return Err(invalid_param()),
        };
        unsafe {
            *index = index_value;
            *mapping = GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT;
        }
        Ok(())
    }
    fn GetPropertyCount(&self) -> windows::core::Result<u32> { Ok(3) }
    fn GetProperty(&self, index: u32) -> windows::core::Result<IPropertyValue> {
        let value = match index {
            0 => PropertyValue::CreateSingle(self.blur_amount)?,
            1 => PropertyValue::CreateUInt32(self.optimization)?,
            2 => PropertyValue::CreateUInt32(self.border_mode)?,
            _ => return Err(invalid_param()),
        };
        value.cast()
    }
    fn GetSource(&self, index: u32) -> windows::core::Result<IGraphicsEffectSource> {
        (index == 0).then(|| self.source.clone()).ok_or_else(invalid_param)
    }
    fn GetSourceCount(&self) -> windows::core::Result<u32> { Ok(1) }
}

/// `ID2D1PathGeometry` is a native Direct2D object and does not implement the
/// WinRT `IGeometrySource2D` interface. CompositionPath requires that WinRT
/// interface, so bridge the native geometry explicitly and expose the
/// `IGeometrySource2DInterop` methods that Composition uses to retrieve it.
#[implement(IGeometrySource2D, IGeometrySource2DInterop)]
struct GeometrySource2D {
    geometry: windows::Win32::Graphics::Direct2D::ID2D1Geometry,
}

impl windows::Graphics::IGeometrySource2D_Impl for GeometrySource2D_Impl {}
impl IGeometrySource2DInterop_Impl for GeometrySource2D_Impl {
    fn GetGeometry(
        &self,
    ) -> windows::core::Result<windows::Win32::Graphics::Direct2D::ID2D1Geometry> {
        Ok(self.geometry.clone())
    }

    fn TryGetGeometryUsingFactory(
        &self,
        _factory: windows::core::Ref<
            windows::Win32::Graphics::Direct2D::ID2D1Factory,
        >,
    ) -> windows::core::Result<windows::Win32::Graphics::Direct2D::ID2D1Geometry> {
        Ok(self.geometry.clone())
    }
}

fn backdrop_tint() -> (Color, f32) {
    let rgb = std::env::var("NEON_BACKDROP_TINT").ok()
        .and_then(|s| s.strip_prefix('#').map(str::to_owned))
        .filter(|s| s.len() == 6)
        .and_then(|s| u32::from_str_radix(&s, 16).ok())
        .map(|v| ((v >> 16) as u8, (v >> 8) as u8, v as u8))
        .unwrap_or((0, 0, 0));
    let opacity = std::env::var("NEON_BACKDROP_TINT_OPACITY").ok()
        .and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.28).clamp(0.0, 1.0);
    (Color { A: 255, R: rgb.0, G: rgb.1, B: rgb.2 }, opacity)
}

/// Resolve the shell polygon in the same local coordinate space as the
/// renderer and SetWindowRgn. The cut order is [bl, br, tr, tl].
fn shell_polygon(bounds: [f32; 4], cut: [f32; 4]) -> [Vector2; 8] {
    let [x, y, width, height] = bounds;
    let [bl, br, tr, tl] = cut.map(|value| value.max(0.0));
    [
        Vector2::new(x + tl, y),
        Vector2::new(x + width - tr, y),
        Vector2::new(x + width, y + tr),
        Vector2::new(x + width, y + height - br),
        Vector2::new(x + width - br, y + height),
        Vector2::new(x + bl, y + height),
        Vector2::new(x, y + height - bl),
        Vector2::new(x, y + tl),
    ]
}

/// Build a WinRT Composition geometric clip from the exact eight-vertex shell
/// polygon. This uses the Direct2D interop required by CompositionPath; no
/// rectangular inset is involved, so zero and asymmetric cuts remain exact.
fn create_shell_clip(
    compositor: &Compositor,
    bounds: [f32; 4],
    cut: [f32; 4],
) -> Result<windows::UI::Composition::CompositionGeometricClip, AcrylicError> {
    let factory: ID2D1Factory = unsafe {
        D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
    }
    .map_err(|e| AcrylicError::Message(format!("D2D1CreateFactory: {e:?}")))?;
    let geometry = unsafe { factory.CreatePathGeometry() }
        .map_err(|e| AcrylicError::Message(format!("CreatePathGeometry: {e:?}")))?;
    let sink = unsafe { geometry.Open() }
        .map_err(|e| AcrylicError::Message(format!("Open path geometry: {e:?}")))?;
    let points = shell_polygon(bounds, cut);
    unsafe {
        sink.SetFillMode(D2D1_FILL_MODE_WINDING);
        sink.BeginFigure(points[0], D2D1_FIGURE_BEGIN_FILLED);
        sink.AddLines(&points[1..]);
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
    }
    unsafe { sink.Close() }
        .map_err(|e| AcrylicError::Message(format!("close path geometry: {e:?}")))?;

    let geometry_source: IGeometrySource2D = GeometrySource2D {
        geometry: geometry
            .cast()
            .map_err(|e| AcrylicError::Message(format!("cast path geometry: {e:?}")))?,
    }
    .into();
    let path = CompositionPath::Create(&geometry_source)
        .map_err(|e| AcrylicError::Message(format!("CompositionPath::Create: {e:?}")))?;
    let path_geometry = compositor
        .CreatePathGeometryWithPath(&path)
        .map_err(|e| AcrylicError::Message(format!("CreatePathGeometryWithPath: {e:?}")))?;
    compositor
        .CreateGeometricClipWithGeometry(&path_geometry)
        .map_err(|e| AcrylicError::Message(format!("CreateGeometricClipWithGeometry: {e:?}")))
}

pub struct AcrylicHost {
    _compositor: Compositor,
    _target: DesktopWindowTarget,
    _effect_brush: CompositionEffectBrush,
    _behind_effect_brush: CompositionEffectBrush,
    _blur_sprite: SpriteVisual,
    _behind_blur_sprite: SpriteVisual,
    _tint_sprite: SpriteVisual,
    _content_brush: CompositionSurfaceBrush,
    _content_sprite: SpriteVisual,
    _behind_brush: CompositionSurfaceBrush,
    _root: ContainerVisual,
    surface_handle: HANDLE,
    behind_surface_handle: HANDLE,
    shell_geometry: std::cell::RefCell<Option<ShellGeometryCache>>,
}

struct ShellGeometryCache {
    bounds: [f32; 4],
    cut: [f32; 4],
    clip: windows::UI::Composition::CompositionGeometricClip,
}

impl Clone for ShellGeometryCache {
    fn clone(&self) -> Self {
        Self {
            bounds: self.bounds,
            cut: self.cut,
            clip: self.clip.clone(),
        }
    }
}

#[derive(Debug)]
pub enum AcrylicError { Message(String) }
impl std::fmt::Display for AcrylicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self::Message(message) = self;
        f.write_str(message)
    }
}
impl std::error::Error for AcrylicError {}

impl AcrylicHost {
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self, AcrylicError> {
        let compositor = Compositor::new().map_err(|e| AcrylicError::Message(format!("Compositor::new: {e:?}")))?;
        let desktop: ICompositorDesktopInterop = compositor.cast().map_err(|e| AcrylicError::Message(format!("desktop interop: {e:?}")))?;
        let target = unsafe { desktop.CreateDesktopWindowTarget(hwnd, false) }.map_err(|e| AcrylicError::Message(format!("desktop target: {e:?}")))?;
        let source: IGraphicsEffectSource = CompositionEffectSourceParameter::Create(&windows::core::HSTRING::from("backdrop"))
            .map_err(|e| AcrylicError::Message(format!("source parameter: {e:?}")))?.cast()
            .map_err(|e| AcrylicError::Message(format!("source cast: {e:?}")))?;
        let effect = GaussianBlurEffect {
            name: std::cell::RefCell::new(windows::core::HSTRING::from("GaussianBlurEffect")),
            blur_amount: std::env::var("NEON_BLUR_AMOUNT").ok().and_then(|s| s.parse().ok()).unwrap_or(8.0),
            optimization: 1, border_mode: 0, source,
        };
        let effect_interface: IGraphicsEffect = effect.into();
        let factory = compositor.CreateEffectFactory(&effect_interface).map_err(|e| AcrylicError::Message(format!("effect factory: {e:?}")))?;
        let effect_brush = factory.CreateBrush().map_err(|e| AcrylicError::Message(format!("effect brush: {e:?}")))?;
        let backdrop_brush = compositor.CreateBackdropBrush().map_err(|e| AcrylicError::Message(format!("backdrop brush: {e:?}")))?;
        effect_brush.SetSourceParameter(&windows::core::HSTRING::from("backdrop"), &backdrop_brush)
            .map_err(|e| AcrylicError::Message(format!("bind backdrop: {e:?}")))?;
        let size = Vector2::new(width.max(1) as f32, height.max(1) as f32);
        let blur_sprite = compositor.CreateSpriteVisual().map_err(|e| AcrylicError::Message(format!("blur visual: {e:?}")))?;
        blur_sprite.SetBrush(&effect_brush).and_then(|_| blur_sprite.SetSize(size)).map_err(|e| AcrylicError::Message(format!("blur visual setup: {e:?}")))?;
        let (tint, opacity) = backdrop_tint();
        let tint_brush = compositor.CreateColorBrushWithColor(tint).map_err(|e| AcrylicError::Message(format!("tint brush: {e:?}")))?;
        let tint_sprite = compositor.CreateSpriteVisual().map_err(|e| AcrylicError::Message(format!("tint visual: {e:?}")))?;
        tint_sprite.SetBrush(&tint_brush).and_then(|_| tint_sprite.SetSize(size)).and_then(|_| tint_sprite.SetOpacity(opacity)).map_err(|e| AcrylicError::Message(format!("tint visual setup: {e:?}")))?;
        let handle = unsafe { DCompositionCreateSurfaceHandle(GENERIC_ALL.0, None) }.map_err(|e| AcrylicError::Message(format!("surface handle: {e:?}")))?;
        let behind_handle = unsafe { DCompositionCreateSurfaceHandle(GENERIC_ALL.0, None) }.map_err(|e| AcrylicError::Message(format!("behind surface handle: {e:?}")))?;
        let interop: ICompositorInterop = compositor.cast().map_err(|e| AcrylicError::Message(format!("compositor interop: {e:?}")))?;
        let surface = unsafe { interop.CreateCompositionSurfaceForHandle(handle) }.map_err(|e| AcrylicError::Message(format!("composition surface: {e:?}")))?;
        let behind_surface = unsafe { interop.CreateCompositionSurfaceForHandle(behind_handle) }.map_err(|e| AcrylicError::Message(format!("behind composition surface: {e:?}")))?;
        let content_brush = compositor.CreateSurfaceBrushWithSurface(&surface).map_err(|e| AcrylicError::Message(format!("content brush: {e:?}")))?;
        let content_sprite = compositor.CreateSpriteVisual().map_err(|e| AcrylicError::Message(format!("content visual: {e:?}")))?;
        content_sprite.SetBrush(&content_brush).and_then(|_| content_sprite.SetSize(size)).map_err(|e| AcrylicError::Message(format!("content visual setup: {e:?}")))?;
        let behind_brush = compositor.CreateSurfaceBrushWithSurface(&behind_surface).map_err(|e| AcrylicError::Message(format!("behind brush: {e:?}")))?;
        let behind_source: IGraphicsEffectSource = CompositionEffectSourceParameter::Create(&windows::core::HSTRING::from("behind"))
            .map_err(|e| AcrylicError::Message(format!("behind source parameter: {e:?}")))?.cast()
            .map_err(|e| AcrylicError::Message(format!("behind source cast: {e:?}")))?;
        let behind_effect = GaussianBlurEffect {
            name: std::cell::RefCell::new(windows::core::HSTRING::from("BehindGlassBlurEffect")),
            blur_amount: std::env::var("NEON_BLUR_AMOUNT").ok().and_then(|s| s.parse().ok()).unwrap_or(8.0),
            optimization: 1, border_mode: 0, source: behind_source,
        };
        let behind_effect_interface: IGraphicsEffect = behind_effect.into();
        let behind_factory = compositor.CreateEffectFactory(&behind_effect_interface).map_err(|e| AcrylicError::Message(format!("behind blur factory: {e:?}")))?;
        let behind_effect_brush = behind_factory.CreateBrush().map_err(|e| AcrylicError::Message(format!("behind blur brush: {e:?}")))?;
        behind_effect_brush.SetSourceParameter(&windows::core::HSTRING::from("behind"), &behind_brush)
            .map_err(|e| AcrylicError::Message(format!("bind behind blur: {e:?}")))?;
        let behind_blur_sprite = compositor.CreateSpriteVisual().map_err(|e| AcrylicError::Message(format!("behind blur visual: {e:?}")))?;
        behind_blur_sprite.SetBrush(&behind_effect_brush).and_then(|_| behind_blur_sprite.SetSize(size)).map_err(|e| AcrylicError::Message(format!("behind blur visual setup: {e:?}")))?;
        let root = compositor.CreateContainerVisual().map_err(|e| AcrylicError::Message(format!("root: {e:?}")))?;
        let children = root.Children().map_err(|e| AcrylicError::Message(format!("root children: {e:?}")))?;
        children.InsertAtTop(&blur_sprite).and_then(|_| children.InsertAtTop(&behind_blur_sprite)).and_then(|_| children.InsertAtTop(&tint_sprite)).and_then(|_| children.InsertAtTop(&content_sprite)).map_err(|e| AcrylicError::Message(format!("root children insert: {e:?}")))?;
        target.SetRoot(&root).map_err(|e| AcrylicError::Message(format!("target root: {e:?}")))?;
        Ok(Self { _compositor: compositor, _target: target, _effect_brush: effect_brush, _behind_effect_brush: behind_effect_brush, _blur_sprite: blur_sprite, _behind_blur_sprite: behind_blur_sprite, _tint_sprite: tint_sprite, _content_brush: content_brush, _content_sprite: content_sprite, _behind_brush: behind_brush, _root: root, surface_handle: handle, behind_surface_handle: behind_handle, shell_geometry: std::cell::RefCell::new(None) })
    }

    pub fn surface_handle(&self) -> HANDLE { self.surface_handle }
    pub fn behind_surface_handle(&self) -> HANDLE { self.behind_surface_handle }

    pub fn set_backdrop_shell_bounds(&self, x: f32, y: f32, width: f32, height: f32, cut: [f32; 4]) -> Result<(), AcrylicError> {
        let bounds = [x, y, width.max(1.0), height.max(1.0)];
        let cached = { self.shell_geometry.borrow().clone() };
        let clip = match cached {
            Some(cached) if cached.bounds == bounds && cached.cut == cut => cached.clip,
            _ => {
                let clip = create_shell_clip(&self._compositor, bounds, cut)?;
                *self.shell_geometry.borrow_mut() = Some(ShellGeometryCache {
                    bounds,
                    cut,
                    clip: clip.clone(),
                });
                clip
            }
        };
        let offset = Vector3::new(x, y, 0.0);
        let size = Vector2::new(width.max(1.0), height.max(1.0));
        self._blur_sprite.SetOffset(offset)
            .and_then(|_| self._blur_sprite.SetSize(size))
            .and_then(|_| self._behind_blur_sprite.SetOffset(offset))
            .and_then(|_| self._behind_blur_sprite.SetSize(size))
            .and_then(|_| self._tint_sprite.SetOffset(offset))
            .and_then(|_| self._tint_sprite.SetSize(size))
            .and_then(|_| self._root.SetClip(&clip))
            .map_err(|e| AcrylicError::Message(format!("set backdrop shell bounds: {e:?}")))
    }

    pub fn resize(&self, width: u32, height: u32) -> Result<(), AcrylicError> {
        let size = Vector2::new(width.max(1) as f32, height.max(1) as f32);
        let cached = { self.shell_geometry.borrow().clone() };
        if let Some(cached) = cached {
            // Reuse the cached path so resize cannot restore a rectangular
            // visual or lose an asymmetric cut.
            let bounds = cached.bounds;
            let clip = cached.clip;
            let offset = Vector3::new(bounds[0], bounds[1], 0.0);
            let shell_size = Vector2::new(bounds[2], bounds[3]);
            self._blur_sprite.SetOffset(offset)
                .and_then(|_| self._blur_sprite.SetSize(shell_size))
                .and_then(|_| self._behind_blur_sprite.SetOffset(offset))
                .and_then(|_| self._behind_blur_sprite.SetSize(shell_size))
                .and_then(|_| self._tint_sprite.SetOffset(offset))
                .and_then(|_| self._tint_sprite.SetSize(shell_size))
                .and_then(|_| self._content_sprite.SetSize(size))
                .and_then(|_| self._root.SetClip(&clip))
        } else {
            self._blur_sprite.SetSize(size)
                .and_then(|_| self._behind_blur_sprite.SetSize(size))
                .and_then(|_| self._tint_sprite.SetSize(size))
                .and_then(|_| self._content_sprite.SetSize(size))
        }
            .map_err(|e| AcrylicError::Message(format!("resize composition visuals: {e:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::shell_polygon;

    #[test]
    fn shell_polygon_preserves_zero_cut_as_full_bounds() {
        let points = shell_polygon([10.0, 20.0, 100.0, 60.0], [0.0; 4]);
        assert_eq!(points[0].X, 10.0);
        assert_eq!(points[0].Y, 20.0);
        assert_eq!(points[2].X, 110.0);
        assert_eq!(points[4].Y, 80.0);
    }

    #[test]
    fn shell_polygon_keeps_asymmetric_cut_order() {
        let points = shell_polygon([10.0, 20.0, 100.0, 60.0], [3.0, 7.0, 11.0, 13.0]);
        assert_eq!((points[0].X, points[0].Y), (23.0, 20.0));
        assert_eq!((points[1].X, points[1].Y), (99.0, 20.0));
        assert_eq!((points[2].X, points[2].Y), (110.0, 31.0));
        assert_eq!((points[5].X, points[5].Y), (13.0, 80.0));
        assert_eq!((points[6].X, points[6].Y), (10.0, 77.0));
    }
}
