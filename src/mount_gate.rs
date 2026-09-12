// SPDX-License-Identifier: GPL-3.0-only
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct State {
    handles: usize,
    detaching: bool,
}
#[derive(Default)]
pub struct MountGate(Mutex<State>);
pub struct OpenGuard(Arc<MountGate>);
impl MountGate {
    pub fn enter(self: &Arc<Self>) -> Result<OpenGuard, &'static str> {
        let mut state = self.0.lock().map_err(|_| "Attachment state unavailable")?;
        if state.detaching {
            return Err("Attachment is being detached");
        }
        state.handles = state
            .handles
            .checked_add(1)
            .ok_or("Too many open handles")?;
        Ok(OpenGuard(self.clone()))
    }
    pub fn begin_detach(&self) -> Result<(), String> {
        let mut state = self.0.lock().map_err(|_| "Attachment state unavailable")?;
        if state.handles != 0 {
            return Err(format!(
                "Local applications still hold {} file or folder handles. Close them and try detaching again.",
                state.handles
            ));
        }
        state.detaching = true;
        Ok(())
    }
}
impl Drop for OpenGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.0.lock() {
            state.handles = state.handles.saturating_sub(1);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn busy_refusal_keeps_opens_working_and_success_closes_the_race() {
        let gate = Arc::new(MountGate::default());
        let first = gate.enter().unwrap();
        assert!(gate.begin_detach().is_err());
        let second = gate.enter().unwrap();
        drop(first);
        assert!(gate.begin_detach().is_err());
        drop(second);
        gate.begin_detach().unwrap();
        assert!(gate.enter().is_err());
    }
}
