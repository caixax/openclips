//! Tolerant property setters. Encoder elements differ in the exact type and
//! spelling of equivalent settings, and setting a property with the wrong
//! type aborts the process, so every write is checked against the property
//! specification first.

use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use tracing::debug;

pub fn set_number(element: &gst::Element, name: &str, value: i64) -> bool {
    let Some(pspec) = element.find_property(name) else {
        return false;
    };
    let t = pspec.value_type();
    if t == glib::Type::U32 {
        element.set_property(name, value.clamp(0, i64::from(u32::MAX)) as u32);
    } else if t == glib::Type::I32 {
        element.set_property(
            name,
            value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        );
    } else if t == glib::Type::U64 {
        element.set_property(name, value.max(0) as u64);
    } else if t == glib::Type::I64 {
        element.set_property(name, value);
    } else if t == glib::Type::F64 {
        element.set_property(name, value as f64);
    } else {
        debug!("property {name} on {} is not numeric", element.name());
        return false;
    }
    true
}

pub fn set_bool(element: &gst::Element, name: &str, value: bool) -> bool {
    match element.find_property(name) {
        Some(pspec) if pspec.value_type() == glib::Type::BOOL => {
            element.set_property(name, value);
            true
        }
        _ => false,
    }
}

/// Sets an enum or flags property by nick, only when the nick exists.
pub fn set_nick(element: &gst::Element, name: &str, nick: &str) -> bool {
    let Some(pspec) = element.find_property(name) else {
        return false;
    };
    let t = pspec.value_type();
    let known = if t.is_a(glib::Type::ENUM) {
        glib::EnumClass::with_type(t).is_some_and(|class| class.value_by_nick(nick).is_some())
    } else if t.is_a(glib::Type::FLAGS) {
        glib::FlagsClass::with_type(t).is_some_and(|class| class.value_by_nick(nick).is_some())
    } else {
        false
    };
    if !known {
        debug!("property {name} on {} has no value {nick}", element.name());
        return false;
    }
    element.set_property_from_str(name, nick);
    true
}
