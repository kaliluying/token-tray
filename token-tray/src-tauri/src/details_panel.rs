use objc2::rc::Retained as ObjcRetained;
use objc2::runtime::AnyObject as ObjcAnyObject;
use objc2_foundation::{NSNumber, NSString, NSUserDefaults, NSValue};
use objc2_quartz_core::{
    CABasicAnimation, CAMediaTiming, CAMediaTimingFunction, CATransform3D, CATransform3DIdentity,
    NSValueCATransform3DAdditions,
};
use tauri::{AppHandle, Manager, WebviewWindow};
use tauri_nspanel::{tauri_panel, ManagerExt, Panel, StyleMask, WebviewWindowExt};

const OPENING_DURATION_SECONDS: f64 = 0.24;
const CLOSING_DURATION_SECONDS: f64 = 0.18;
const OPENING_SCALE: f64 = 0.965;
const CLOSING_SCALE: f64 = 0.97;
const OPENING_TRANSLATION_Y: f64 = 7.0;
const CLOSING_TRANSLATION_Y: f64 = 6.0;

tauri_panel! {
    panel!(DetailsPanel {
        config: {
            can_become_key_window: true,
            can_become_main_window: false,
            is_floating_panel: true,
            hides_on_deactivate: false
        }
    })
}

#[derive(Clone, Copy)]
enum Transition {
    Opening,
    Closing,
}

/// Converts the existing Tauri details window into a real NSPanel and applies the
/// Space/fullscreen behavior that a menu-bar popover needs.
pub fn initialize(details: &WebviewWindow) -> Result<(), String> {
    let panel = details
        .to_panel::<DetailsPanel>()
        .map_err(|error| format!("无法创建详情面板: {error}"))?;

    panel.set_hides_on_deactivate(false);
    panel.set_level(objc2_app_kit::NSScreenSaverWindowLevel as i64);
    panel
        .add_style_mask(StyleMask::empty().nonactivating_panel().into())
        .map_err(|error| format!("无法配置详情面板样式: {error}"))?;

    let current_behavior = panel.as_panel().collectionBehavior();
    panel.set_collection_behavior(super::fullscreen_collection_behavior(current_behavior));
    prepare_content_layer(panel.as_panel());
    Ok(())
}

pub fn initialize_if_needed(details: &WebviewWindow) -> Result<(), String> {
    if details.app_handle().get_webview_panel("details").is_err() {
        initialize(details)?;
    }
    Ok(())
}

pub fn show(app: &AppHandle, details: &WebviewWindow) -> Result<(), String> {
    let panel = app
        .get_webview_panel("details")
        .map_err(|_| "找不到原生详情面板".to_string())?;
    let panel_for_main_thread = panel.clone();

    details
        .run_on_main_thread(move || animate(&*panel_for_main_thread, Transition::Opening))
        .map_err(|error| error.to_string())
}

pub fn begin_close(app: &AppHandle, details: &WebviewWindow) -> Result<(), String> {
    let panel = app
        .get_webview_panel("details")
        .map_err(|_| "找不到原生详情面板".to_string())?;
    let panel_for_main_thread = panel.clone();

    details
        .run_on_main_thread(move || animate(&*panel_for_main_thread, Transition::Closing))
        .map_err(|error| error.to_string())
}

pub fn hide(app: &AppHandle, details: &WebviewWindow) -> Result<(), String> {
    let panel = app
        .get_webview_panel("details")
        .map_err(|_| "找不到原生详情面板".to_string())?;
    let panel_for_main_thread = panel.clone();

    details
        .run_on_main_thread(move || {
            panel_for_main_thread.hide();
            reset_content_layer(panel_for_main_thread.as_panel());
        })
        .map_err(|error| error.to_string())
}

fn animate(panel: &dyn Panel, transition: Transition) {
    let content_view = panel.content_view();
    content_view.setWantsLayer(true);
    let Some(layer) = content_view.layer() else {
        panel.show_and_make_key();
        return;
    };

    layer.removeAllAnimations();

    if reduced_motion_enabled() {
        match transition {
            Transition::Opening => {
                layer.setOpacity(1.0);
                layer.setTransform(identity_transform());
                panel.show_and_make_key();
            }
            Transition::Closing => {
                layer.setOpacity(0.0);
                layer.setTransform(closing_transform());
            }
        }
        return;
    }

    match transition {
        Transition::Opening => {
            let from_transform = opening_transform();
            let identity = identity_transform();
            layer.setOpacity(0.0);
            layer.setTransform(from_transform);
            panel.show_and_make_key();
            layer.setOpacity(1.0);
            layer.setTransform(identity);
            layer.addAnimation_forKey(
                &transform_animation(
                    from_transform,
                    identity,
                    OPENING_DURATION_SECONDS,
                    (0.16, 1.0, 0.3, 1.0),
                ),
                Some(&NSString::from_str("details-panel-transform")),
            );
            layer.addAnimation_forKey(
                &opacity_animation(0.0, 1.0, OPENING_DURATION_SECONDS, (0.16, 1.0, 0.3, 1.0)),
                Some(&NSString::from_str("details-panel-opacity")),
            );
        }
        Transition::Closing => {
            let identity = identity_transform();
            let to_transform = closing_transform();
            layer.setOpacity(1.0);
            layer.setTransform(identity);
            layer.setOpacity(0.0);
            layer.setTransform(to_transform);
            layer.addAnimation_forKey(
                &transform_animation(
                    identity,
                    to_transform,
                    CLOSING_DURATION_SECONDS,
                    (0.4, 0.0, 1.0, 1.0),
                ),
                Some(&NSString::from_str("details-panel-transform")),
            );
            layer.addAnimation_forKey(
                &opacity_animation(1.0, 0.0, CLOSING_DURATION_SECONDS, (0.4, 0.0, 1.0, 1.0)),
                Some(&NSString::from_str("details-panel-opacity")),
            );
        }
    }
}

fn reduced_motion_enabled() -> bool {
    let defaults = NSUserDefaults::standardUserDefaults();
    defaults.boolForKey(&NSString::from_str("AppleReduceMotion"))
}

fn prepare_content_layer(panel: &objc2_app_kit::NSPanel) {
    let Some(content_view) = panel.contentView() else {
        return;
    };
    content_view.setWantsLayer(true);
    if let Some(layer) = content_view.layer() {
        layer.setOpacity(1.0);
        layer.setTransform(identity_transform());
    }
}

fn reset_content_layer(panel: &objc2_app_kit::NSPanel) {
    let Some(content_view) = panel.contentView() else {
        return;
    };
    content_view.setWantsLayer(true);
    if let Some(layer) = content_view.layer() {
        layer.removeAllAnimations();
        layer.setOpacity(1.0);
        layer.setTransform(identity_transform());
    }
}

fn opening_transform() -> CATransform3D {
    identity_transform()
        .scale(OPENING_SCALE, OPENING_SCALE, 1.0)
        .translate(0.0, OPENING_TRANSLATION_Y, 0.0)
}

fn closing_transform() -> CATransform3D {
    identity_transform()
        .scale(CLOSING_SCALE, CLOSING_SCALE, 1.0)
        .translate(0.0, CLOSING_TRANSLATION_Y, 0.0)
}

fn identity_transform() -> CATransform3D {
    unsafe { CATransform3DIdentity }
}

fn timing_function(control_points: (f32, f32, f32, f32)) -> ObjcRetained<CAMediaTimingFunction> {
    CAMediaTimingFunction::functionWithControlPoints(
        control_points.0,
        control_points.1,
        control_points.2,
        control_points.3,
    )
}

fn transform_animation(
    from: CATransform3D,
    to: CATransform3D,
    duration: f64,
    control_points: (f32, f32, f32, f32),
) -> ObjcRetained<CABasicAnimation> {
    let animation = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("transform")));
    animation.setDuration(duration);
    animation.setTimingFunction(Some(&timing_function(control_points)));
    let from_value = unsafe { NSValue::valueWithCATransform3D(from) };
    let to_value = unsafe { NSValue::valueWithCATransform3D(to) };
    unsafe {
        animation.setFromValue(Some(&*from_value as &ObjcAnyObject));
        animation.setToValue(Some(&*to_value as &ObjcAnyObject));
    }
    animation
}

fn opacity_animation(
    from: f32,
    to: f32,
    duration: f64,
    control_points: (f32, f32, f32, f32),
) -> ObjcRetained<CABasicAnimation> {
    let animation = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("opacity")));
    animation.setDuration(duration);
    animation.setTimingFunction(Some(&timing_function(control_points)));
    let from_value = NSNumber::new_f32(from);
    let to_value = NSNumber::new_f32(to);
    unsafe {
        animation.setFromValue(Some(&*from_value as &ObjcAnyObject));
        animation.setToValue(Some(&*to_value as &ObjcAnyObject));
    }
    animation
}

#[cfg(test)]
mod tests {
    use super::{closing_transform, identity_transform, opening_transform};

    #[test]
    fn native_transitions_start_from_a_smaller_translated_panel() {
        let opening = opening_transform();
        let closing = closing_transform();

        assert!(opening.m11 < identity_transform().m11);
        assert!(opening.m22 < identity_transform().m22);
        assert!(opening.m42 > 0.0);
        assert!(closing.m11 < identity_transform().m11);
        assert!(closing.m22 < identity_transform().m22);
        assert!(closing.m42 > 0.0);
    }
}
