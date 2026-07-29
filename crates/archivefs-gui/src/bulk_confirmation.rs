pub const TYPED_CONFIRMATION_THRESHOLD: usize = 25;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConfirmationState {
    #[default]
    AwaitingConfirmation,
    Cancelled,
    Confirmed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmationValidation {
    NoItems,
    PreviewRequired,
    Ready,
    TypedCountRequired {
        expected: usize,
    },
    InvalidTypedCount {
        expected: usize,
        entered: Option<usize>,
    },
    AlreadyCancelled,
    AlreadyConfirmed,
}

/// UI-independent safety state for a bulk operation. It owns no items and
/// executes nothing; callers retain the exact preview and action payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BulkConfirmation {
    item_count: usize,
    preview_complete: bool,
    typed_count: String,
    state: ConfirmationState,
}

impl BulkConfirmation {
    pub fn new(item_count: usize) -> Self {
        Self {
            item_count,
            preview_complete: false,
            typed_count: String::new(),
            state: ConfirmationState::AwaitingConfirmation,
        }
    }

    pub const fn item_count(&self) -> usize {
        self.item_count
    }

    pub const fn preview_complete(&self) -> bool {
        self.preview_complete
    }

    pub const fn requires_typed_count(&self) -> bool {
        self.item_count > TYPED_CONFIRMATION_THRESHOLD
    }

    pub const fn state(&self) -> ConfirmationState {
        self.state
    }

    pub fn typed_count(&self) -> &str {
        &self.typed_count
    }

    pub fn mark_preview_complete(&mut self) {
        if self.state == ConfirmationState::AwaitingConfirmation {
            self.preview_complete = true;
        }
    }

    pub fn set_typed_count(&mut self, value: impl Into<String>) {
        if self.state == ConfirmationState::AwaitingConfirmation {
            self.typed_count = value.into();
        }
    }

    pub fn validation(&self) -> ConfirmationValidation {
        match self.state {
            ConfirmationState::Cancelled => return ConfirmationValidation::AlreadyCancelled,
            ConfirmationState::Confirmed => return ConfirmationValidation::AlreadyConfirmed,
            ConfirmationState::AwaitingConfirmation => {}
        }
        if self.item_count == 0 {
            return ConfirmationValidation::NoItems;
        }
        if !self.preview_complete {
            return ConfirmationValidation::PreviewRequired;
        }
        if !self.requires_typed_count() {
            return ConfirmationValidation::Ready;
        }
        if self.typed_count.trim().is_empty() {
            return ConfirmationValidation::TypedCountRequired {
                expected: self.item_count,
            };
        }
        let entered = self.typed_count.trim().parse().ok();
        if entered == Some(self.item_count) {
            ConfirmationValidation::Ready
        } else {
            ConfirmationValidation::InvalidTypedCount {
                expected: self.item_count,
                entered,
            }
        }
    }

    pub fn confirm(&mut self) -> Result<(), ConfirmationValidation> {
        let validation = self.validation();
        if validation == ConfirmationValidation::Ready {
            self.state = ConfirmationState::Confirmed;
            Ok(())
        } else {
            Err(validation)
        }
    }

    pub fn cancel(&mut self) {
        if self.state == ConfirmationState::AwaitingConfirmation {
            self.state = ConfirmationState::Cancelled;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_items_can_never_be_confirmed() {
        let mut confirmation = BulkConfirmation::new(0);
        confirmation.mark_preview_complete();
        assert_eq!(confirmation.confirm(), Err(ConfirmationValidation::NoItems));
        assert_eq!(
            confirmation.state(),
            ConfirmationState::AwaitingConfirmation
        );
    }

    #[test]
    fn one_item_requires_preview_then_normal_confirmation() {
        let mut confirmation = BulkConfirmation::new(1);
        assert_eq!(
            confirmation.validation(),
            ConfirmationValidation::PreviewRequired
        );
        confirmation.mark_preview_complete();
        assert!(!confirmation.requires_typed_count());
        assert_eq!(confirmation.confirm(), Ok(()));
        assert_eq!(confirmation.state(), ConfirmationState::Confirmed);
    }

    #[test]
    fn twenty_five_is_the_normal_confirmation_boundary() {
        let mut confirmation = BulkConfirmation::new(25);
        confirmation.mark_preview_complete();
        assert!(!confirmation.requires_typed_count());
        assert_eq!(confirmation.validation(), ConfirmationValidation::Ready);
    }

    #[test]
    fn twenty_six_requires_the_exact_typed_count() {
        let mut confirmation = BulkConfirmation::new(26);
        confirmation.mark_preview_complete();
        assert!(confirmation.requires_typed_count());
        assert_eq!(
            confirmation.validation(),
            ConfirmationValidation::TypedCountRequired { expected: 26 }
        );
        confirmation.set_typed_count("25");
        assert_eq!(
            confirmation.validation(),
            ConfirmationValidation::InvalidTypedCount {
                expected: 26,
                entered: Some(25)
            }
        );
        confirmation.set_typed_count("26");
        assert_eq!(confirmation.confirm(), Ok(()));
    }

    #[test]
    fn malformed_and_large_counts_are_safe() {
        let mut confirmation = BulkConfirmation::new(1_000_000);
        confirmation.mark_preview_complete();
        confirmation.set_typed_count("many");
        assert_eq!(
            confirmation.confirm(),
            Err(ConfirmationValidation::InvalidTypedCount {
                expected: 1_000_000,
                entered: None
            })
        );
        confirmation.set_typed_count("1000000");
        assert_eq!(confirmation.confirm(), Ok(()));
    }

    #[test]
    fn cancellation_is_terminal() {
        let mut confirmation = BulkConfirmation::new(3);
        confirmation.cancel();
        confirmation.mark_preview_complete();
        assert_eq!(confirmation.state(), ConfirmationState::Cancelled);
        assert_eq!(
            confirmation.confirm(),
            Err(ConfirmationValidation::AlreadyCancelled)
        );
    }
}
