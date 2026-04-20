/// Sound effect identifiers — each maps to a .wav file in assets/audio/sfx/.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SoundEffect {
    FootstepWalk,
    FootstepRun,
    QteCorrect,
    QteWrong,
    QteRoundComplete,
    QteSessionComplete,
    UrgencyWarning,
    UrgencyCritical,
    Victory,
    GameOver,
}

impl SoundEffect {
    pub fn path(self) -> &'static str {
        match self {
            Self::FootstepWalk      => "assets/audio/sfx/footstep_walk.wav",
            Self::FootstepRun       => "assets/audio/sfx/footstep_run.wav",
            Self::QteCorrect        => "assets/audio/sfx/qte_correct.wav",
            Self::QteWrong          => "assets/audio/sfx/qte_wrong.wav",
            Self::QteRoundComplete  => "assets/audio/sfx/qte_round_complete.wav",
            Self::QteSessionComplete => "assets/audio/sfx/qte_session_complete.wav",
            Self::UrgencyWarning    => "assets/audio/sfx/urgency_warning.wav",
            Self::UrgencyCritical   => "assets/audio/sfx/urgency_critical.wav",
            Self::Victory           => "assets/audio/sfx/victory.wav",
            Self::GameOver          => "assets/audio/sfx/game_over.wav",
        }
    }

    /// Minimum seconds between consecutive plays of this effect.
    pub fn cooldown(self) -> f32 {
        match self {
            // Footstep cadence is controlled externally; no internal cooldown.
            Self::FootstepWalk | Self::FootstepRun => 0.0,
            Self::QteCorrect | Self::QteWrong => 0.05,
            Self::QteRoundComplete | Self::QteSessionComplete => 0.1,
            // Prevent re-triggering if urgency fluctuates near the threshold.
            Self::UrgencyWarning | Self::UrgencyCritical => 5.0,
            Self::Victory | Self::GameOver => 0.0,
        }
    }

    /// Default per-effect gain (0 = silent, max_gain = slider top).
    pub fn default_gain(self) -> f32 {
        match self {
            Self::FootstepWalk       => 0.55,
            Self::FootstepRun        => 0.65,
            Self::QteCorrect         => 0.10,
            Self::QteWrong           => 0.20,
            Self::QteRoundComplete   => 0.20,
            Self::QteSessionComplete => 0.20,
            Self::UrgencyWarning     => 0.50,
            Self::UrgencyCritical    => 0.70,
            Self::Victory            => 0.70,
            Self::GameOver           => 0.70,
        }
    }

    /// Ceiling for this effect's slider — keeps the useful range centered.
    pub fn max_gain(self) -> f32 {
        match self {
            Self::FootstepWalk | Self::FootstepRun => 1.0,
            Self::QteCorrect   | Self::QteWrong    => 0.5,
            Self::QteRoundComplete                 => 0.7,
            Self::QteSessionComplete               => 0.8,
            Self::UrgencyWarning                   => 0.8,
            Self::UrgencyCritical                  => 1.0,
            Self::Victory | Self::GameOver         => 1.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::FootstepWalk       => "Footstep walk",
            Self::FootstepRun        => "Footstep run",
            Self::QteCorrect         => "QTE correct",
            Self::QteWrong           => "QTE wrong",
            Self::QteRoundComplete   => "QTE round",
            Self::QteSessionComplete => "QTE session",
            Self::UrgencyWarning     => "Urgency warn",
            Self::UrgencyCritical    => "Urgency crit",
            Self::Victory            => "Victory",
            Self::GameOver           => "Game over",
        }
    }
}

/// Music track identifiers — each maps to an .ogg file in assets/audio/music/.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MusicTrack {
    MainTheme,
    TenseLoop,
    VictoryStinger,
    DefeatStinger,
}

impl MusicTrack {
    pub fn path(self) -> &'static str {
        match self {
            Self::MainTheme      => "assets/audio/music/main_theme.wav",
            Self::TenseLoop      => "assets/audio/music/tense_loop.wav",
            Self::VictoryStinger => "assets/audio/music/victory_stinger.wav",
            Self::DefeatStinger  => "assets/audio/music/defeat_stinger.wav",
        }
    }

    pub fn looped(self) -> bool {
        matches!(self, Self::MainTheme | Self::TenseLoop)
    }

    /// Default per-track gain.
    pub fn default_gain(self) -> f32 {
        match self {
            Self::MainTheme      => 0.10,
            Self::TenseLoop      => 0.20,
            Self::VictoryStinger => 0.50,
            Self::DefeatStinger  => 0.50,
        }
    }

    /// Slider ceiling — music is mixed far below SFX.
    pub fn max_gain(self) -> f32 {
        match self {
            Self::MainTheme      => 0.3,
            Self::TenseLoop      => 0.5,
            Self::VictoryStinger => 0.8,
            Self::DefeatStinger  => 0.8,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::MainTheme      => "Main theme",
            Self::TenseLoop      => "Tense loop",
            Self::VictoryStinger => "Victory",
            Self::DefeatStinger  => "Defeat",
        }
    }
}
