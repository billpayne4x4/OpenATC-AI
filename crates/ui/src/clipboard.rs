//! System clipboard shared with applications outside the simulator.

/// Keeps ownership of the native clipboard for the lifetime of the UI.
#[derive(Default)]
pub struct Backend {
    clipboard: Option<arboard::Clipboard>,
    local: String,
}

impl Backend {
    fn clipboard(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        self.clipboard.as_mut()
    }
}

impl imgui::ClipboardBackend for Backend {
    fn get(&mut self) -> Option<String> {
        self.clipboard()
            .and_then(|clipboard| clipboard.get_text().ok())
            .or_else(|| Some(self.local.clone()))
    }

    fn set(&mut self, value: &str) {
        value.clone_into(&mut self.local);
        if let Some(clipboard) = self.clipboard() {
            let _ = clipboard.set_text(value);
        }
    }
}
