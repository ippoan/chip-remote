//! The real [`ChipDriver`]: `chip_uia::Uia` (UI Automation on Claude desktop).

use std::time::Duration;

use chip_core::protocol::Action;
use chip_core::{ActionError, ChipInfo, Labels};

use crate::driver::{ChipDriver, DriverFactory};

impl ChipDriver for chip_uia::Uia {
    fn list_chips(&self) -> Result<Vec<ChipInfo>, ActionError> {
        chip_uia::Uia::list_chips(self)
    }

    fn act(
        &self,
        title: &str,
        tldr: Option<&str>,
        session_title: Option<&str>,
        action: Action,
        raise_wait: Duration,
    ) -> Result<(), ActionError> {
        chip_uia::Uia::act(self, title, tldr, session_title, action, raise_wait)
    }
}

/// Creates `chip_uia::Uia` on the driver thread.
pub struct UiaFactory;

impl DriverFactory for UiaFactory {
    type Driver = chip_uia::Uia;

    fn create(&mut self, labels: &Labels) -> Result<chip_uia::Uia, ActionError> {
        chip_uia::Uia::new(labels.clone())
    }

    fn keep_awake(&mut self) -> bool {
        chip_uia::keep_awake()
    }
}
