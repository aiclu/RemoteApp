Based on Slint 1.17.1 from crates.io (original license and copyright retained).

RemoteAPP patch: replace the Android MotionAction::Scroll todo! with a PointerScrolled event using native horizontal/vertical axes. Without this patch, a physical mouse wheel can panic. This is a target-specific dependency patch; upgrade or remove it once upstream provides the equivalent behavior.
