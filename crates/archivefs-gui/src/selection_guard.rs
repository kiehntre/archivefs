/// Exact selection plus the generation at which asynchronous work began.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionToken<K> {
    selection: K,
    generation: u64,
}

impl<K> SelectionToken<K> {
    pub fn selection(&self) -> &K {
        &self.selection
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

/// A result that remains readable only while its exact selection token is
/// current. This complements existing request/provider tokens; it does not
/// replace or weaken them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionBound<K, T> {
    token: SelectionToken<K>,
    value: T,
}

impl<K, T> SelectionBound<K, T> {
    pub fn token(&self) -> &SelectionToken<K> {
        &self.token
    }

    pub fn value(&self) -> &T {
        &self.value
    }

    pub fn into_value(self) -> T {
        self.value
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionGuard<K> {
    selected: Option<K>,
    generation: u64,
}

impl<K> Default for SelectionGuard<K> {
    fn default() -> Self {
        Self {
            selected: None,
            generation: 0,
        }
    }
}

impl<K: Clone + Eq> SelectionGuard<K> {
    pub fn selected(&self) -> Option<&K> {
        self.selected.as_ref()
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns true only for an exact selection change. Callers can use the
    /// return value to clear unbound legacy presentation state immediately.
    pub fn update(&mut self, selected: Option<K>) -> bool {
        if self.selected == selected {
            return false;
        }
        self.selected = selected;
        self.generation = self
            .generation
            .checked_add(1)
            .expect("selection generation exhausted");
        true
    }

    pub fn token(&self) -> Option<SelectionToken<K>> {
        Some(SelectionToken {
            selection: self.selected.clone()?,
            generation: self.generation,
        })
    }

    pub fn is_current(&self, token: &SelectionToken<K>) -> bool {
        self.generation == token.generation && self.selected.as_ref() == Some(&token.selection)
    }

    /// Accepts a completed async value only for the same exact selection and
    /// generation that launched it.
    pub fn bind_if_current<T>(
        &self,
        token: SelectionToken<K>,
        value: T,
    ) -> Option<SelectionBound<K, T>> {
        self.is_current(&token)
            .then_some(SelectionBound { token, value })
    }

    /// Clears a cached identity, Cheats & Mods result, or other presentation
    /// slot if it belongs to a previous selection/generation.
    pub fn clear_stale<T>(&self, slot: &mut Option<SelectionBound<K, T>>) -> bool {
        if slot
            .as_ref()
            .is_some_and(|result| !self.is_current(result.token()))
        {
            *slot = None;
            true
        } else {
            false
        }
    }

    pub fn current_value<'a, T>(&self, slot: &'a SelectionBound<K, T>) -> Option<&'a T> {
        self.is_current(slot.token()).then_some(slot.value())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_change_clears_identity_and_cheat_slots() {
        let mut guard = SelectionGuard::default();
        assert!(guard.update(Some("game-a")));
        let identity_token = guard.token().unwrap();
        let cheat_token = guard.token().unwrap();
        let mut identity = guard.bind_if_current(identity_token, "identity-a");
        let mut cheats = guard.bind_if_current(cheat_token, "cheats-a");

        assert!(guard.update(Some("game-b")));
        assert!(guard.clear_stale(&mut identity));
        assert!(guard.clear_stale(&mut cheats));
        assert!(identity.is_none());
        assert!(cheats.is_none());
    }

    #[test]
    fn game_a_async_result_is_rejected_under_game_b() {
        let mut guard = SelectionGuard::default();
        guard.update(Some("game-a"));
        let game_a_request = guard.token().unwrap();
        guard.update(Some("game-b"));

        assert_eq!(guard.bind_if_current(game_a_request, "late result"), None);
    }

    #[test]
    fn selecting_the_same_game_preserves_current_results() {
        let mut guard = SelectionGuard::default();
        guard.update(Some("game-a"));
        let generation = guard.generation();
        let mut result = guard.bind_if_current(guard.token().unwrap(), 42);

        assert!(!guard.update(Some("game-a")));
        assert_eq!(guard.generation(), generation);
        assert!(!guard.clear_stale(&mut result));
        assert_eq!(guard.current_value(result.as_ref().unwrap()), Some(&42));
    }

    #[test]
    fn returning_to_game_a_does_not_revive_its_old_generation() {
        let mut guard = SelectionGuard::default();
        guard.update(Some("game-a"));
        let old_game_a = guard.token().unwrap();
        guard.update(Some("game-b"));
        guard.update(Some("game-a"));

        assert_eq!(guard.selected(), Some(&"game-a"));
        assert!(!guard.is_current(&old_game_a));
    }

    #[test]
    fn clearing_selection_invalidates_in_flight_results() {
        let mut guard = SelectionGuard::default();
        guard.update(Some("game-a"));
        let request = guard.token().unwrap();
        guard.update(None);

        assert!(guard.token().is_none());
        assert_eq!(guard.bind_if_current(request, "late result"), None);
    }
}
