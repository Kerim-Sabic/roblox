use std::collections::BTreeSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrongFreezeSignal {
    FramesStalled,
    WindowUnresponsive,
    InputHeartbeatLost,
    ConfirmedDisconnectDialog,
    ProcessCpuStalled,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeakFreezeSignal {
    HoneyInactive,
    NavigationFailed,
    OcrUncertain,
}

#[derive(Clone, Debug, Default)]
pub struct FreezeEvidence {
    /// How long the current evidence has persisted continuously.
    pub observed_for: Duration,
    pub strong: BTreeSet<StrongFreezeSignal>,
    pub weak: BTreeSet<WeakFreezeSignal>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreezeDecision {
    Healthy,
    Suspected,
    RestartAllowed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FreezeAssessment {
    pub decision: FreezeDecision,
    pub independent_strong_signals: usize,
    pub reason: String,
}

#[derive(Clone, Copy, Debug)]
pub struct FreezeClassifier {
    pub minimum_observation: Duration,
    pub required_independent_signals: usize,
}

impl Default for FreezeClassifier {
    fn default() -> Self {
        Self {
            minimum_observation: Duration::from_secs(30),
            required_independent_signals: 2,
        }
    }
}

impl FreezeClassifier {
    #[must_use]
    pub fn classify(self, evidence: &FreezeEvidence) -> FreezeAssessment {
        let strong_count = evidence.strong.len();
        let old_enough = evidence.observed_for >= self.minimum_observation;
        let decision = if strong_count >= self.required_independent_signals && old_enough {
            FreezeDecision::RestartAllowed
        } else if strong_count > 0 || !evidence.weak.is_empty() {
            FreezeDecision::Suspected
        } else {
            FreezeDecision::Healthy
        };
        let reason = match decision {
            FreezeDecision::Healthy => "no freeze evidence is active".to_owned(),
            FreezeDecision::Suspected if !old_enough => format!(
                "evidence is younger than the {} second confirmation window",
                self.minimum_observation.as_secs()
            ),
            FreezeDecision::Suspected => format!(
                "restart refused: {strong_count} independent strong signal(s), {} required",
                self.required_independent_signals
            ),
            FreezeDecision::RestartAllowed => format!(
                "restart permitted after {strong_count} independent strong signals persisted for {} seconds",
                evidence.observed_for.as_secs()
            ),
        };
        FreezeAssessment {
            decision,
            independent_strong_signals: strong_count,
            reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weak_or_single_signals_never_authorize_restart() {
        let classifier = FreezeClassifier::default();
        let weak = FreezeEvidence {
            observed_for: Duration::from_secs(120),
            weak: [WeakFreezeSignal::HoneyInactive].into_iter().collect(),
            ..FreezeEvidence::default()
        };
        assert_eq!(
            classifier.classify(&weak).decision,
            FreezeDecision::Suspected
        );

        let single = FreezeEvidence {
            observed_for: Duration::from_secs(120),
            strong: [StrongFreezeSignal::FramesStalled].into_iter().collect(),
            ..FreezeEvidence::default()
        };
        assert_eq!(
            classifier.classify(&single).decision,
            FreezeDecision::Suspected
        );
    }

    #[test]
    fn two_independent_persistent_signals_allow_restart() {
        let evidence = FreezeEvidence {
            observed_for: Duration::from_secs(45),
            strong: [
                StrongFreezeSignal::FramesStalled,
                StrongFreezeSignal::WindowUnresponsive,
            ]
            .into_iter()
            .collect(),
            ..FreezeEvidence::default()
        };

        assert_eq!(
            FreezeClassifier::default().classify(&evidence).decision,
            FreezeDecision::RestartAllowed
        );
    }
}
