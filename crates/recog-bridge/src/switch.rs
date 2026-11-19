//! The one place the bridge moves the storage to another meeting.
//!
//! The live side keeps the audio writers of the current meeting, and the storage
//! knows nothing of them: a switch of the stored meeting that the writers did not
//! follow leaves the new meeting's segments pointing into the old meeting's audio.
//! So the rest of the bridge sees the storage as a [`StoreHandle`], which has no
//! switch, and switches through [`Bridge::switch_stored_meeting`].

use std::path::PathBuf;
use std::sync::Arc;

use recog_core::pyjson::Value;

use crate::bridge::Bridge;
use crate::hooks::MeetingStore;

/// The storage without its meeting switches (`on_meeting_join`, `on_ktalk_leave`),
/// which only [`Bridge::switch_stored_meeting`] reaches.
#[derive(Clone)]
pub(crate) struct StoreHandle(Arc<dyn MeetingStore>);

impl StoreHandle {
    pub(crate) fn new(store: Arc<dyn MeetingStore>) -> Self {
        Self(store)
    }

    pub(crate) fn current_meeting_id(&self) -> Option<String> {
        self.0.current_meeting_id()
    }

    pub(crate) fn current_meeting_path(&self) -> Option<PathBuf> {
        self.0.current_meeting_path()
    }

    /// # Errors
    /// As [`MeetingStore::load_meta`].
    pub(crate) fn load_meta(&self) -> Result<Value, String> {
        self.0.load_meta()
    }

    pub(crate) fn is_conference_source(&self, source: Option<&str>) -> bool {
        self.0.is_conference_source(source)
    }

    /// # Errors
    /// As [`MeetingStore::update_title`].
    pub(crate) fn update_title(&self, title: &str) -> Result<(), String> {
        self.0.update_title(title)
    }

    /// # Errors
    /// As [`MeetingStore::get_current_transcript`].
    pub(crate) fn get_current_transcript(&self) -> Result<Vec<Value>, String> {
        self.0.get_current_transcript()
    }
}

/// A switch of the stored meeting that the live side's writers followed.
pub(crate) struct Switched<T> {
    /// What the switch answered.
    pub(crate) answer: T,
    /// The storage moved to another meeting directory.
    pub(crate) moved: bool,
}

/// Why a switch of the stored meeting failed.
pub(crate) enum SwitchError {
    /// The switch itself failed; the writers were not asked to follow.
    Storage(String),
    /// The storage switched and the live side's writers could not follow.
    Writers(String),
}

impl From<SwitchError> for String {
    fn from(e: SwitchError) -> Self {
        match e {
            SwitchError::Storage(e) | SwitchError::Writers(e) => e,
        }
    }
}

impl Bridge {
    /// Moves the storage to another meeting with the live side around it: the live
    /// side drops the in-flight chunks of the old meeting, `switch` moves the storage
    /// (and does what has to happen before the writers move), and the live side's
    /// writers follow. They follow when the meeting directory changed, and always
    /// under `ktalk_only`, where they open with the conference; in an unchanged
    /// directory they are already in place.
    ///
    /// # Errors
    /// [`SwitchError::Storage`] is what `switch` answered, [`SwitchError::Writers`]
    /// what the live side's `meeting_changed` answered.
    pub(crate) fn switch_stored_meeting<T>(
        &self,
        storage: &StoreHandle,
        switch: impl FnOnce(&dyn MeetingStore) -> Result<T, String>,
    ) -> Result<Switched<T>, SwitchError> {
        let old_path = storage.current_meeting_path();
        let live = self.wired().live;
        if let Some(live) = &live {
            live.before_meeting_switch();
        }
        let answer = switch(storage.0.as_ref()).map_err(SwitchError::Storage)?;
        let path = storage.current_meeting_path();
        let moved = path != old_path;
        if (moved || self.config.recording_policy.gated())
            && let Some(live) = &live
        {
            live.meeting_changed(storage.current_meeting_id().as_deref(), path.as_deref())
                .map_err(SwitchError::Writers)?;
        }
        Ok(Switched { answer, moved })
    }
}
