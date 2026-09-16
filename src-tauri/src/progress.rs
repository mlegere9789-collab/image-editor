//! Progress and cancellation for long operations.
//!
//! A generation, a Super Zoom, or a tiled edit can run for a minute on
//! a CPU. Rather than stall the app until it is done, such an operation
//! reports how far along it is through a [`Progress`] and stops — with
//! [`CANCELLED`] — the moment the report comes back refused. The core
//! stays independent of how the report reaches the user: the desktop
//! app streams it over a Tauri channel and refuses it once the user
//! presses Cancel; tests record it; everything else passes [`Silent`].

/// The error a cancelled operation returns. The app shows it as a quiet
/// notice rather than a failure.
pub const CANCELLED: &str = "Cancelled.";

/// Where a long operation reports to, and where it learns to stop.
pub trait Progress {
    /// `done` of `total` units of `stage` are complete. Returns
    /// `Err(CANCELLED)` when the operation should stop; the operation
    /// returns that error unchanged and leaves its output unfinished.
    fn report(&mut self, stage: &str, done: usize, total: usize) -> Result<(), String>;
}

/// A [`Progress`] that nobody is listening to and that never cancels.
pub struct Silent;

impl Progress for Silent {
    fn report(&mut self, _stage: &str, _done: usize, _total: usize) -> Result<(), String> {
        Ok(())
    }
}

/// Where one operation's reports fall within a larger one: a tile's
/// `steps` denoising steps are `base..base + steps` of the whole
/// edit's `total`, so the bar moves smoothly across tiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub base: usize,
    pub total: usize,
}

impl Span {
    /// A span that is the whole operation: `total` units from zero.
    pub fn whole(total: usize) -> Self {
        Span { base: 0, total }
    }

    /// Reports `done` units of this span as progress on the whole.
    pub fn report(
        &self,
        progress: &mut dyn Progress,
        stage: &str,
        done: usize,
    ) -> Result<(), String> {
        progress.report(stage, (self.base + done).min(self.total), self.total)
    }
}

/// A [`Progress`] that remembers every report and, if asked, cancels at
/// the `cancel_at`th one — what a test hands an operation.
#[derive(Default)]
pub struct Recorder {
    pub reports: Vec<(String, usize, usize)>,
    pub cancel_at: Option<usize>,
}

impl Recorder {
    /// A recorder that refuses its `n`th report (counting from one).
    pub fn cancelling_at(n: usize) -> Self {
        Recorder {
            reports: Vec::new(),
            cancel_at: Some(n),
        }
    }
}

impl Progress for Recorder {
    fn report(&mut self, stage: &str, done: usize, total: usize) -> Result<(), String> {
        self.reports.push((stage.to_string(), done, total));
        match self.cancel_at {
            Some(n) if self.reports.len() >= n => Err(CANCELLED.to_string()),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorder_keeps_every_report_and_cancels_where_asked() {
        let mut silent = Silent;
        assert!(silent.report("x", 1, 1).is_ok());

        let mut recorder = Recorder::default();
        assert!(recorder.report("Generating", 1, 3).is_ok());
        assert!(recorder.report("Generating", 3, 3).is_ok());
        assert_eq!(
            recorder.reports,
            vec![
                ("Generating".to_string(), 1, 3),
                ("Generating".to_string(), 3, 3)
            ]
        );

        let mut cancelling = Recorder::cancelling_at(2);
        assert!(cancelling.report("Super Zoom", 1, 4).is_ok());
        assert_eq!(
            cancelling.report("Super Zoom", 2, 4),
            Err(CANCELLED.to_string())
        );
        assert_eq!(cancelling.reports.len(), 2);
    }

    #[test]
    fn a_span_places_its_reports_inside_the_whole() {
        let mut recorder = Recorder::default();
        let tile = Span {
            base: 10,
            total: 30,
        };
        tile.report(&mut recorder, "Tile", 3).unwrap();
        tile.report(&mut recorder, "Tile", 25).unwrap();
        assert_eq!(
            recorder.reports,
            vec![("Tile".to_string(), 13, 30), ("Tile".to_string(), 30, 30)]
        );
        assert_eq!(Span::whole(5), Span { base: 0, total: 5 });
        let mut cancelling = Recorder::cancelling_at(1);
        assert_eq!(
            tile.report(&mut cancelling, "Tile", 1),
            Err(CANCELLED.to_string())
        );
    }
}
