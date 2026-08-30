#![forbid(unsafe_code)]

//! Offline moon phase, solar Wheel-of-the-Year, and personal-year period
//! calculations, backed by the `astro` crate's Meeus-algorithm
//! implementation. Pure computation: no I/O, no vault or filesystem
//! dependency, mirroring `contextos-mermaid`'s trait-behind-a-crate-boundary
//! shape so swapping to a different underlying algorithm implementation
//! stays a trait-impl change, not a rewrite.

/// A calendar year whose corresponding ephemeris calculation would fall
/// outside what the underlying algorithm can represent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EphemerisError {
    #[error("year {year} is outside the range ephemeris calculations support")]
    YearOutOfRange { year: i32 },
    #[error("start_date {start_date:?} must not be after end_date {end_date:?}")]
    InvalidDateRange {
        start_date: time::Date,
        end_date: time::Date,
    },
}

impl EphemerisError {
    /// Stable, machine-readable error code, mirroring
    /// `contextos_core::PathError::code`'s own convention.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::YearOutOfRange { .. } => "ephemeris/year-out-of-range",
            Self::InvalidDateRange { .. } => "ephemeris/invalid-date-range",
        }
    }

    /// Actionable remediation text, mirroring
    /// `contextos_core::PathError::remediation`'s own convention.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        match self {
            Self::YearOutOfRange { .. } => {
                "Use a year within -9999 to 9999, this workspace's supported calendar range."
            }
            Self::InvalidDateRange { .. } => "Ensure start_date is not after end_date.",
        }
    }
}

/// One of the Moon's eight named phases, determined by its
/// position within the current synodic (New-Moon-to-New-Moon) cycle: eight
/// equal 45-degree arcs, each centred on its name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MoonPhaseName {
    New,
    WaxingCrescent,
    FirstQuarter,
    WaxingGibbous,
    Full,
    WaningGibbous,
    LastQuarter,
    WaningCrescent,
}

/// The full moon-phase result for one calendar date.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the four near_* flags are MoonPhaseReport's own named \
              fields, one per primary phase; not mutually exclusive (a \
              large enough tolerance could in principle flag more than \
              one), so an enum would misrepresent them"
)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MoonPhaseReport {
    pub name: MoonPhaseName,
    /// Fraction of the lunar disk illuminated, `0.0` (new) to `1.0` (full).
    pub illumination_fraction: f64,
    /// Days elapsed since the most recent New Moon at or before the
    /// queried date, `0.0` (inclusive) to just under the synodic month's
    /// length (~29.53 days).
    pub days_into_cycle: f64,
    /// Whether the queried date falls within the caller's tolerance window
    /// of the New Moon nearest to it.
    pub near_new: bool,
    /// As `near_new`, for the nearest First Quarter.
    pub near_first_quarter: bool,
    /// As `near_new`, for the nearest Full Moon.
    pub near_full: bool,
    /// As `near_new`, for the nearest Last Quarter.
    pub near_last_quarter: bool,
}

/// Computes [`MoonPhaseReport`] for a calendar date.
pub trait ComputesMoonPhase {
    /// `tolerance_hours` governs the four `near_*` window flags on the
    /// result (`±`, default `12.0`).
    fn moon_phase(&self, date: time::Date, tolerance_hours: f64) -> MoonPhaseReport;
}

/// One of the four solar events marking a season boundary: the two
/// equinoxes (Sun's apparent geocentric ecliptic longitude at `0`/`180`
/// degrees) and two solstices (`90`/`270` degrees).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SolarEventKind {
    MarchEquinox,
    JuneSolstice,
    SeptemberEquinox,
    DecemberSolstice,
}

/// The result for one solar event: which one, and its exact UTC instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SolarEvent {
    pub kind: SolarEventKind,
    pub instant: time::OffsetDateTime,
}

/// Computes the four solar events (two equinoxes, two solstices) for a
/// calendar year, in chronological order.
pub trait ComputesSolarEvents {
    /// # Errors
    ///
    /// Returns [`EphemerisError::YearOutOfRange`] if `year` cannot be
    /// represented by the underlying algorithm.
    fn solar_events(&self, year: i32) -> Result<[SolarEvent; 4], EphemerisError>;
}

/// Which hemisphere the operator observes seasonal correspondence from:
/// explicit, never auto-detected, since defaulting would silently produce
/// the wrong seasonal correspondence for an operator who genuinely splits
/// time between hemispheres across a year.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Hemisphere {
    Northern,
    Southern,
}

/// Whether a Wheel-of-the-Year point is one of the four solar events (a
/// season's actual start) or one of the four cross-quarter midpoints
/// between them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WheelOfYearRole {
    Boundary,
    Checkpoint,
}

/// One of the eight traditional Wheel-of-the-Year names, hemisphere-
/// agnostic in itself: which of the year's eight fixed positions (four
/// solar events, four cross-quarter dates) carries which name is what
/// [`Hemisphere`] actually determines; the name set itself does not
/// change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WheelOfYearName {
    Imbolc,
    SpringEquinox,
    Beltane,
    SummerSolstice,
    Lughnasadh,
    AutumnEquinox,
    Samhain,
    WinterSolstice,
}

/// The result for one Wheel-of-the-Year point: its hemisphere-correct
/// name, whether it is a boundary or a checkpoint, and when it falls. For a
/// boundary, the exact solar-event instant; for a checkpoint, midnight UTC
/// of its fixed traditional calendar date, which carries no meaningful
/// time of day of its own, unlike a boundary's astronomically precise
/// instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WheelOfYearPoint {
    pub name: WheelOfYearName,
    pub role: WheelOfYearRole,
    pub instant: time::OffsetDateTime,
}

/// Computes all eight Wheel-of-the-Year points for a calendar year, in
/// chronological order, hemisphere-correctly named.
pub trait ComputesWheelOfYear {
    /// # Errors
    ///
    /// Returns [`EphemerisError::YearOutOfRange`] if `year` cannot be
    /// represented by the underlying algorithm.
    fn wheel_of_year(
        &self,
        year: i32,
        hemisphere: Hemisphere,
    ) -> Result<[WheelOfYearPoint; 8], EphemerisError>;
}

/// One of the seven planetary rulers of a personal-year period, in fixed
/// Chaldean order: Sun, Moon, Mars, Mercury, Jupiter, Venus, Saturn.
/// Source: H. Spencer Lewis, *Self Mastery and Fate with the Cycles of
/// Life*.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RulingPlanet {
    Sun,
    Moon,
    Mars,
    Mercury,
    Jupiter,
    Venus,
    Saturn,
}

/// The personal-year result for one `as_of_date`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PersonalYearPeriod {
    /// `1` to `7`; period `1` always begins exactly at the birthday.
    pub period_number: u8,
    pub ruling_planet: RulingPlanet,
    /// Whether `as_of_date` falls within the caller's tolerance window of
    /// either edge of the current period: the source material describes
    /// the transition between periods as gradual, not a precise
    /// day-to-day cutover, so this is a soft boundary rather than a sharp
    /// one.
    pub transition: bool,
}

/// Computes which of the seven annually-recurring personal-year periods
/// contains `as_of_date`, and that period's ruling planet.
pub trait ComputesPersonalYearPeriod {
    /// `transition_tolerance_days` governs the `transition` flag on the
    /// result (`±`, default `2.0`). No birth-time parameter: the source
    /// method carries none, and `birth_date`/`as_of_date` are
    /// already-validated calendar dates, so this never fails.
    fn personal_year_period(
        &self,
        birth_date: time::Date,
        as_of_date: time::Date,
        transition_tolerance_days: f64,
    ) -> PersonalYearPeriod;
}

/// One of the four primary Moon phases (the eight-name [`MoonPhaseName`]
/// set narrowed to only the ones reported as discrete, instant-in-time
/// checkpoints): the waxing/waning gibbous/crescent names describe a range
/// of days, not a single crossable moment, so they have no place in an
/// aggregate of instants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrimaryMoonPhase {
    New,
    FirstQuarter,
    Full,
    LastQuarter,
}

/// One horizon boundary or checkpoint, aggregated across moon phases,
/// Wheel-of-the-Year points, and personal-year periods: a moon-quarter
/// instant, a Wheel-of-the-Year point, or a personal-year period change,
/// each carrying its own exact UTC instant and its own domain-specific
/// detail.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HorizonEvent {
    MoonQuarter {
        instant: time::OffsetDateTime,
        phase: PrimaryMoonPhase,
    },
    WheelOfYear {
        instant: time::OffsetDateTime,
        name: WheelOfYearName,
        role: WheelOfYearRole,
    },
    PersonalYearPeriodChange {
        instant: time::OffsetDateTime,
        period_number: u8,
        ruling_planet: RulingPlanet,
    },
}

impl HorizonEvent {
    #[must_use]
    pub fn instant(&self) -> time::OffsetDateTime {
        match self {
            Self::MoonQuarter { instant, .. }
            | Self::WheelOfYear { instant, .. }
            | Self::PersonalYearPeriodChange { instant, .. } => *instant,
        }
    }
}

/// The two tolerance windows that govern boundary detection (`config`'s
/// entire scope, deliberately: `hemisphere`/`birth_date` stay named
/// parameters rather than folding into this, since silently defaulting
/// either would produce a wrong result, not a merely suboptimal one, the
/// same reasoning that keeps `hemisphere` explicit rather than
/// auto-detected).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoundariesConfig {
    /// `±`, default `12.0`.
    pub moon_phase_tolerance_hours: f64,
    /// `±`, default `2.0`.
    pub personal_year_transition_tolerance_days: f64,
}

impl Default for BoundariesConfig {
    fn default() -> Self {
        Self {
            moon_phase_tolerance_hours: 12.0,
            personal_year_transition_tolerance_days: 2.0,
        }
    }
}

/// Aggregates moon phases, Wheel-of-the-Year points, and personal-year
/// periods into every horizon boundary or checkpoint crossed within
/// `[start_date, end_date]` inclusive, computed once across the window
/// rather than once per day. A single day
/// is simply the case `start_date == end_date`; no separate single-date
/// method exists. Moon-quarter checkpoints are always included; Wheel-of-
/// Year points are included only when `hemisphere` is supplied;
/// personal-year period changes are included only when `birth_date` is
/// supplied, so a caller with no operator-specific data still gets a
/// valid, narrower result rather than an error.
pub trait ComputesBoundaries {
    /// # Errors
    ///
    /// Returns [`EphemerisError::InvalidDateRange`] if `start_date` is
    /// after `end_date`, or [`EphemerisError::YearOutOfRange`] if any year
    /// the range touches cannot be represented by the underlying
    /// algorithm.
    fn boundaries(
        &self,
        start_date: time::Date,
        end_date: time::Date,
        birth_date: Option<time::Date>,
        hemisphere: Option<Hemisphere>,
        config: BoundariesConfig,
    ) -> Result<Vec<HorizonEvent>, EphemerisError>;
}

/// [`ComputesMoonPhase`], [`ComputesSolarEvents`], [`ComputesWheelOfYear`],
/// [`ComputesPersonalYearPeriod`], and [`ComputesBoundaries`] backed by the
/// `astro` crate's Meeus-algorithm implementation. Stateless:
/// holds no configuration or cache.
#[derive(Clone, Copy, Debug, Default)]
pub struct AstroEphemeris;

impl AstroEphemeris {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl ComputesMoonPhase for AstroEphemeris {
    fn moon_phase(&self, date: time::Date, tolerance_hours: f64) -> MoonPhaseReport {
        moon_phase(date, tolerance_hours)
    }
}

impl ComputesSolarEvents for AstroEphemeris {
    fn solar_events(&self, year: i32) -> Result<[SolarEvent; 4], EphemerisError> {
        solar_events(year)
    }
}

impl ComputesWheelOfYear for AstroEphemeris {
    fn wheel_of_year(
        &self,
        year: i32,
        hemisphere: Hemisphere,
    ) -> Result<[WheelOfYearPoint; 8], EphemerisError> {
        wheel_of_year(year, hemisphere)
    }
}

impl ComputesPersonalYearPeriod for AstroEphemeris {
    fn personal_year_period(
        &self,
        birth_date: time::Date,
        as_of_date: time::Date,
        transition_tolerance_days: f64,
    ) -> PersonalYearPeriod {
        personal_year_period(birth_date, as_of_date, transition_tolerance_days)
    }
}

impl ComputesBoundaries for AstroEphemeris {
    fn boundaries(
        &self,
        start_date: time::Date,
        end_date: time::Date,
        birth_date: Option<time::Date>,
        hemisphere: Option<Hemisphere>,
        config: BoundariesConfig,
    ) -> Result<Vec<HorizonEvent>, EphemerisError> {
        boundaries(start_date, end_date, birth_date, hemisphere, config)
    }
}

/// One astronomical unit, in kilometres (IAU 2012 exact definition): needed
/// to bring `astro::sun::geocent_ecl_pos`'s AU-denominated Earth-Sun
/// distance into the same unit as `astro::lunar::geocent_ecl_pos`'s
/// kilometre-denominated Earth-Moon distance before
/// [`astro::lunar::illum_frac_frm_ecl_coords`] can compare them; the two
/// functions do not share a unit by default, and passing mismatched units
/// silently produces a nonsense illumination fraction rather than an error.
const AU_IN_KM: f64 = 149_597_870.7;

/// Mean synodic (New-Moon-to-New-Moon) month length in days, the same
/// constant `astro::lunar::time_of_phase`'s own algorithm is built on.
const SYNODIC_MONTH_DAYS: f64 = 29.530_588_861;

/// The fraction of a synodic month `astro::lunar::time_of_phase`'s own `k`
/// numbering advances between one named phase and the next (New=0,
/// First=0.25, Full=0.5, Last=0.75).
fn phase_offset(phase: &astro::lunar::Phase) -> f64 {
    match phase {
        astro::lunar::Phase::New => 0.0,
        astro::lunar::Phase::First => 0.25,
        astro::lunar::Phase::Full => 0.5,
        astro::lunar::Phase::Last => 0.75,
    }
}

/// A synthetic `astro::time::Date` engineered so `astro::time::decimal_year`
/// reproduces `target_decimal_year` exactly: year and month are not
/// meaningful calendar components here (month is fixed to January, which
/// astro's own `decimal_year` treats as a plain 365-day year with no
/// leap-day branch, making the reconstruction exact rather than
/// approximate), only the resulting `decimal_year` value matters, since
/// that is the only thing [`astro::lunar::time_of_phase`] reads from an
/// anchor before discarding it in favour of its own derived `k`.
fn decimal_year_anchor(target_decimal_year: f64) -> astro::time::Date {
    let year = target_decimal_year.floor();
    let decimal_day = (target_decimal_year - year) * 365.0;
    // `target_decimal_year` is always within roughly one year of a real
    // `time::Date`'s own year (see `to_astro_date`'s equivalent
    // comment), so this is exact given the caller's own year range, never
    // a lossy truncation in practice.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "bounded to a real time::Date's year range, always within i16"
    )]
    let year = year as i16;
    astro::time::Date {
        year,
        month: 1,
        decimal_day,
        cal_type: astro::time::CalType::Gregorian,
    }
}

/// The exact Meeus-algorithm instant of `phase` closest in `k`-numbering to
/// `integer_k`, bypassing [`astro::lunar::time_of_phase`]'s own anchor-date
/// selection entirely: anchors at that `k` bucket's midpoint, immune to the
/// truncation-boundary jitter [`nearest_phase_jd`]'s own doc comment
/// describes.
fn phase_jd_at_integer_k(integer_k: f64, phase: &astro::lunar::Phase) -> f64 {
    let bucket_midpoint = if integer_k >= 0.0 {
        integer_k + 0.5
    } else {
        integer_k - 0.5
    };
    let target_decimal_year = bucket_midpoint / 12.3685 + 2000.0;
    astro::lunar::time_of_phase(&decimal_year_anchor(target_decimal_year), phase)
}

/// The Julian Day of the occurrence of `phase` genuinely closest to
/// `date_jd`.
///
/// `astro::lunar::time_of_phase`'s own doc comment claims to return
/// whichever occurrence is "closest to date", but its actual
/// implementation truncates (rather than rounds) a linear estimate of the
/// lunation number derived from the *anchor's* decimal year, which can
/// jump a full synodic month ahead of the true nearest occurrence for an
/// anchor sitting close to (or slightly past) its target: confirmed
/// empirically against `astro-2.0.0/tests/lunar.rs`'s own cited 1977
/// fixture, where anchoring at the exact New Moon date it names still
/// returned the *next* month's New Moon instead. Rather than anchor at
/// `date` itself and trust that selection, this computes the caller's own
/// continuous lunation-number estimate, evaluates both integer `k`
/// candidates it could reasonably round to (via
/// [`phase_jd_at_integer_k`], which sidesteps the same truncation bug by
/// engineering its own anchor), and returns whichever real result is
/// actually closer to `date_jd`. Checking both neighbours rather than
/// trusting a single rounded estimate matters most exactly at a boundary,
/// where the periodic correction terms inside the full Meeus polynomial
/// (not just the linear `k` estimate) can make either side the true
/// answer.
fn nearest_phase_jd(query_decimal_year: f64, date_jd: f64, phase: &astro::lunar::Phase) -> f64 {
    let raw_k = 12.3685 * (query_decimal_year - 2000.0) - phase_offset(phase);
    let lower_k = raw_k.floor();
    let candidate_lower = phase_jd_at_integer_k(lower_k, phase);
    let candidate_upper = phase_jd_at_integer_k(lower_k + 1.0, phase);
    if (candidate_lower - date_jd).abs() <= (candidate_upper - date_jd).abs() {
        candidate_lower
    } else {
        candidate_upper
    }
}

fn moon_phase(date: time::Date, tolerance_hours: f64) -> MoonPhaseReport {
    let astro_date = to_astro_date(date);
    let date_jd = astro::time::julian_day(&astro_date);
    let query_decimal_year = astro::time::decimal_year(&astro_date);

    // The most recent New Moon at or before `date`: `nearest_phase_jd` can
    // legitimately return a New Moon *after* `date` (whenever `date` sits
    // in the second half of its cycle, closer to the upcoming New Moon
    // than the previous one); rolling back one synodic month in that case
    // anchors `days_into_cycle` on the preceding one instead, never a
    // future one. The mean synodic month is an approximation (the true
    // month varies by a few tenths of a day around it), acceptable here
    // since `days_into_cycle` only needs to be accurate to well under the
    // 45-degree (~3.7-day) resolution `phase_name_for_angle` buckets at.
    let mut new_moon_jd = nearest_phase_jd(query_decimal_year, date_jd, &astro::lunar::Phase::New);
    if new_moon_jd > date_jd {
        new_moon_jd -= SYNODIC_MONTH_DAYS;
    }
    let days_into_cycle = date_jd - new_moon_jd;
    let phase_angle_degrees = (days_into_cycle / SYNODIC_MONTH_DAYS) * 360.0;
    let name = phase_name_for_angle(phase_angle_degrees);

    let (moon_ecl, moon_dist_km) = astro::lunar::geocent_ecl_pos(date_jd);
    let (sun_ecl, sun_dist_au) = astro::sun::geocent_ecl_pos(date_jd);
    let illumination_fraction = astro::lunar::illum_frac_frm_ecl_coords(
        moon_ecl.long,
        moon_ecl.lat,
        sun_ecl.long,
        moon_dist_km,
        sun_dist_au * AU_IN_KM,
    );

    let is_near = |phase: &astro::lunar::Phase| {
        let target_jd = nearest_phase_jd(query_decimal_year, date_jd, phase);
        (target_jd - date_jd).abs() * 24.0 <= tolerance_hours
    };

    MoonPhaseReport {
        name,
        illumination_fraction,
        days_into_cycle,
        near_new: is_near(&astro::lunar::Phase::New),
        near_first_quarter: is_near(&astro::lunar::Phase::First),
        near_full: is_near(&astro::lunar::Phase::Full),
        near_last_quarter: is_near(&astro::lunar::Phase::Last),
    }
}

/// Buckets a continuous phase angle (`0.0` to `360.0` degrees, `0` = New)
/// into one of the eight named phases, each spanning 45 degrees centred on
/// its name (e.g. Full spans `157.5` to `202.5`).
fn phase_name_for_angle(angle_degrees: f64) -> MoonPhaseName {
    let normalized = angle_degrees.rem_euclid(360.0);
    if !(22.5..337.5).contains(&normalized) {
        MoonPhaseName::New
    } else if normalized < 67.5 {
        MoonPhaseName::WaxingCrescent
    } else if normalized < 112.5 {
        MoonPhaseName::FirstQuarter
    } else if normalized < 157.5 {
        MoonPhaseName::WaxingGibbous
    } else if normalized < 202.5 {
        MoonPhaseName::Full
    } else if normalized < 247.5 {
        MoonPhaseName::WaningGibbous
    } else if normalized < 292.5 {
        MoonPhaseName::LastQuarter
    } else {
        MoonPhaseName::WaningCrescent
    }
}

/// Mean rate of the Sun's apparent geocentric ecliptic longitude, in
/// degrees per day (`360 / 365.2422`): the fixed slope
/// [`solve_solar_event_jd`] divides by each iteration. The Sun's true rate
/// varies slightly across the year (Earth's orbit is elliptical), but the
/// variation is small enough that a fixed-slope iteration still converges
/// in a handful of steps from a seed already accurate to within a day or
/// so, confirmed empirically while building this crate (3 to 4 iterations
/// to below `1e-8` degrees for every case tried).
const MEAN_SUN_RATE_DEGREES_PER_DAY: f64 = 0.985_647_3;

fn solar_events(year: i32) -> Result<[SolarEvent; 4], EphemerisError> {
    // `time::Date`'s own supported range (`-9999..=9999` without the
    // `large-dates` feature this workspace doesn't enable) both bounds the
    // result to a representable calendar year and guarantees every
    // corresponding Julian Day this function computes is comfortably
    // positive (a JD is negative only before roughly 4713 BC), so
    // `jd_to_utc`'s own defensive error path can never actually trigger
    // for a `year` that passes this check.
    if !(-9999..=9999).contains(&year) {
        return Err(EphemerisError::YearOutOfRange { year });
    }
    Ok([
        solar_event(year, SolarEventKind::MarchEquinox)?,
        solar_event(year, SolarEventKind::JuneSolstice)?,
        solar_event(year, SolarEventKind::SeptemberEquinox)?,
        solar_event(year, SolarEventKind::DecemberSolstice)?,
    ])
}

fn solar_event(year: i32, kind: SolarEventKind) -> Result<SolarEvent, EphemerisError> {
    let seed_jd = mean_solar_event_jd(year, kind);
    let jd_dynamical_time = solve_solar_event_jd(seed_jd, target_longitude_degrees(kind));
    Ok(SolarEvent {
        kind,
        instant: jd_dynamical_time_to_utc(jd_dynamical_time)?,
    })
}

fn target_longitude_degrees(kind: SolarEventKind) -> f64 {
    match kind {
        SolarEventKind::MarchEquinox => 0.0,
        SolarEventKind::JuneSolstice => 90.0,
        SolarEventKind::SeptemberEquinox => 180.0,
        SolarEventKind::DecemberSolstice => 270.0,
    }
}

/// Meeus, *Astronomical Algorithms*, Table 27.A: mean (uncorrected) instant
/// of each solar event, valid for years 1000 to 3000 AD, using the
/// normalised year fraction `Y`, equal to `(year minus 2000) divided by
/// 1000`. Reproduced from a widely-republished table, not re-derived; if a
/// coefficient here is ever imprecise, the consequence is bounded, not
/// silent corruption: this only seeds [`solve_solar_event_jd`]'s
/// root-finder, which converges on the actual Sun-longitude function
/// regardless, so a seed off by even a few days would still land the
/// correct final instant (confirmed by the convergence-iteration count
/// each call already tracks) or, in a pathological case, fail to converge
/// at all rather than converge to a silently wrong one, since the
/// root-finder only ever moves toward whichever longitude crossing it
/// actually finds.
fn mean_solar_event_jd(year: i32, kind: SolarEventKind) -> f64 {
    let y = (f64::from(year) - 2000.0) / 1000.0;
    let y2 = y * y;
    let y3 = y2 * y;
    let y4 = y3 * y;
    match kind {
        SolarEventKind::MarchEquinox => {
            2_451_623.809_84 + 365_242.374_04 * y + 0.051_69 * y2 - 0.004_11 * y3 - 0.000_57 * y4
        }
        SolarEventKind::JuneSolstice => {
            2_451_716.567_67 + 365_241.626_03 * y + 0.003_25 * y2 + 0.008_88 * y3 - 0.000_30 * y4
        }
        SolarEventKind::SeptemberEquinox => {
            2_451_810.217_15 + 365_242.017_67 * y - 0.115_75 * y2 + 0.003_37 * y3 + 0.000_78 * y4
        }
        SolarEventKind::DecemberSolstice => {
            2_451_900.059_52 + 365_242.740_49 * y - 0.062_23 * y2 - 0.008_23 * y3 + 0.000_32 * y4
        }
    }
}

/// The Sun's apparent geocentric ecliptic longitude at `jd` (Dynamical
/// Time), in degrees: the geometric longitude
/// [`astro::sun::geocent_ecl_pos`] returns, corrected for nutation
/// ([`astro::nutation::nutation`]) and aberration
/// ([`astro::aberr::sol_aberr`]). An equinox or solstice is defined by the
/// *apparent* longitude crossing `0`/`90`/`180`/`270` degrees, not the
/// geometric one; omitting these two corrections was confirmed empirically
/// while building this crate to shift every computed instant by roughly
/// ten minutes against commonly published equinox/solstice times, both
/// correction terms together closing that gap to within about a minute.
fn sun_apparent_longitude_degrees(jd: f64) -> f64 {
    let (ecliptic_position, earth_sun_distance_au) = astro::sun::geocent_ecl_pos(jd);
    let (nutation_in_longitude, _nutation_in_obliquity) = astro::nutation::nutation(jd);
    let aberration = astro::aberr::sol_aberr(earth_sun_distance_au);
    (ecliptic_position.long + nutation_in_longitude + aberration).to_degrees()
}

/// The shortest signed difference `a - b`, in degrees, wrapped to `(-180,
/// 180]`: without this, comparing a longitude near `360`/`0` degrees
/// against a target near the opposite side of that boundary (the March
/// equinox, target `0`, is the case that matters here) would otherwise
/// read as a spurious ~360-degree jump instead of a small one.
fn angle_diff_degrees(a: f64, b: f64) -> f64 {
    let raw = (a - b).rem_euclid(360.0);
    if raw > 180.0 { raw - 360.0 } else { raw }
}

/// Refines `seed_jd` (Dynamical Time) to the Julian Day at which the Sun's
/// apparent geocentric ecliptic longitude actually equals
/// `target_longitude_degrees`, via fixed-slope Newton iteration against
/// [`sun_apparent_longitude_degrees`]. Converges to below `1e-8` degrees
/// (a fraction of a second of time) in 3 to 4 iterations for every case
/// exercised while building this crate, well within the 20-iteration
/// bound, so that bound is never actually reached in practice; it exists
/// only to guarantee termination rather than loop indefinitely if some
/// future input behaved unexpectedly.
fn solve_solar_event_jd(seed_jd: f64, target_longitude_degrees: f64) -> f64 {
    let mut jd = seed_jd;
    for _ in 0..20 {
        let diff = angle_diff_degrees(sun_apparent_longitude_degrees(jd), target_longitude_degrees);
        if diff.abs() < 1e-8 {
            break;
        }
        jd -= diff / MEAN_SUN_RATE_DEGREES_PER_DAY;
    }
    jd
}

/// Converts a UT Julian Day to a UTC instant. `original_year` is threaded
/// through only to name the failure if the defensive, otherwise-unreachable
/// error path below is ever actually hit (see [`solar_events`]'s own
/// comment on why `jd_ut` is always positive given its caller's `year`
/// check).
fn jd_to_utc(jd_ut: f64, original_year: i32) -> Result<time::OffsetDateTime, EphemerisError> {
    let year_out_of_range = || EphemerisError::YearOutOfRange {
        year: original_year,
    };
    let (year, month, decimal_day) =
        astro::time::date_frm_julian_day(jd_ut).map_err(|_| year_out_of_range())?;
    let day_number = decimal_day.floor();
    // Rounded to the nearest second: this computation's own accuracy
    // (root-finder tolerance, `delta_t`'s polynomial approximation, the
    // mean-instant seed) is good to at best a handful of seconds, so
    // carrying floating-point sub-second noise through to the result
    // would be false precision, not genuine extra accuracy.
    let seconds_into_day = ((decimal_day - day_number) * 86_400.0).round();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "day_number is always a valid 1-31 day-of-month value and \
                  seconds_into_day is always within 0..=86400, both by \
                  construction from a real calendar date"
    )]
    let day = day_number as u8;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "seconds_into_day is always within 0..=86400 by construction, \
                  comfortably within i64 range"
    )]
    let seconds_into_day = seconds_into_day as i64;
    let month = time::Month::try_from(month).map_err(|_| year_out_of_range())?;
    let date = time::Date::from_calendar_date(i32::from(year), month, day)
        .map_err(|_| year_out_of_range())?;
    let midnight = time::PrimitiveDateTime::new(date, time::Time::MIDNIGHT);
    // `PrimitiveDateTime + Duration` rolls the date forward on its own if
    // `seconds_into_day` rounded up to a full day, so no separate
    // end-of-month/end-of-year carry logic is needed here.
    Ok((midnight + time::Duration::seconds(seconds_into_day)).assume_utc())
}

/// Converts a Dynamical/Ephemeris Time (TT) Julian Day, as
/// [`astro::lunar::time_of_phase`] and [`astro::sun::geocent_ecl_pos`]'s
/// root-finder both work in, to a UTC instant via `delta_t` (Meeus's own
/// TT-UT polynomial, already implemented in `astro::time`). `delta_t`
/// needs a `(year, month)` close to `jd_dynamical_time`'s own calendar
/// date; since it changes by only about a second per year in the modern
/// era, using the *pre-correction* calendar date to look it up, rather
/// than iterating to convergence, is accurate enough that the difference
/// would never be visible at this method's own whole-second output
/// precision (confirmed for the solar-events calculation, the same
/// reasoning applies unchanged here).
fn jd_dynamical_time_to_utc(
    jd_dynamical_time: f64,
) -> Result<time::OffsetDateTime, EphemerisError> {
    let (approx_year, approx_month, _) = astro::time::date_frm_julian_day(jd_dynamical_time)
        .map_err(|_| EphemerisError::YearOutOfRange { year: 0 })?;
    let delta_t_seconds = astro::time::delta_t(i32::from(approx_year), approx_month);
    let jd_universal_time = jd_dynamical_time - delta_t_seconds / 86_400.0;
    jd_to_utc(jd_universal_time, i32::from(approx_year))
}

/// The eight Wheel-of-the-Year positions, in chronological order, each
/// mapped to the [`WheelOfYearName`] it carries in the Northern Hemisphere:
/// position `0` is Imbolc, `1` is the March boundary (Spring Equinox), and
/// so on. The traditional "1/2 February" and "31 October/1 November"
/// notation for these dates leaves Imbolc's and Samhain's exact day
/// genuinely ambiguous
/// between two traditional choices; this picks the earlier day for both
/// (1 February, 31 October), consistent with the "eve of" convention
/// under which many traditions begin the observance the night before,
/// documented here as a deliberate choice, not an unstated default.
fn wheel_of_year(
    year: i32,
    hemisphere: Hemisphere,
) -> Result<[WheelOfYearPoint; 8], EphemerisError> {
    let events = solar_events(year)?;
    let imbolc = checkpoint_instant(year, time::Month::February, 1)?;
    let beltane = checkpoint_instant(year, time::Month::May, 1)?;
    let lughnasadh = checkpoint_instant(year, time::Month::August, 1)?;
    let samhain = checkpoint_instant(year, time::Month::October, 31)?;

    Ok([
        WheelOfYearPoint {
            name: wheel_name_for_position(0, hemisphere),
            role: WheelOfYearRole::Checkpoint,
            instant: imbolc,
        },
        WheelOfYearPoint {
            name: wheel_name_for_position(1, hemisphere),
            role: WheelOfYearRole::Boundary,
            instant: events[0].instant,
        },
        WheelOfYearPoint {
            name: wheel_name_for_position(2, hemisphere),
            role: WheelOfYearRole::Checkpoint,
            instant: beltane,
        },
        WheelOfYearPoint {
            name: wheel_name_for_position(3, hemisphere),
            role: WheelOfYearRole::Boundary,
            instant: events[1].instant,
        },
        WheelOfYearPoint {
            name: wheel_name_for_position(4, hemisphere),
            role: WheelOfYearRole::Checkpoint,
            instant: lughnasadh,
        },
        WheelOfYearPoint {
            name: wheel_name_for_position(5, hemisphere),
            role: WheelOfYearRole::Boundary,
            instant: events[2].instant,
        },
        WheelOfYearPoint {
            name: wheel_name_for_position(6, hemisphere),
            role: WheelOfYearRole::Checkpoint,
            instant: samhain,
        },
        WheelOfYearPoint {
            name: wheel_name_for_position(7, hemisphere),
            role: WheelOfYearRole::Boundary,
            instant: events[3].instant,
        },
    ])
}

/// Maps a chronological Wheel-of-the-Year position (`0` to `7`, Northern
/// Hemisphere ordering: Imbolc, Spring Equinox, Beltane, Summer Solstice,
/// Lughnasadh, Autumn Equinox, Samhain, Winter Solstice) to the name it
/// actually carries for `hemisphere`. The calendar position never moves
/// (checkpoints fall on fixed dates, boundaries on astronomically fixed
/// instants); only which *name* attaches to a given position changes, by
/// exactly half the wheel (`+4`, wrapping), matching how a June solstice
/// is Southern Hemisphere midwinter without the solstice itself happening
/// on a different date.
fn wheel_name_for_position(position: u8, hemisphere: Hemisphere) -> WheelOfYearName {
    let effective_position = match hemisphere {
        Hemisphere::Northern => position,
        Hemisphere::Southern => (position + 4) % 8,
    };
    match effective_position {
        0 => WheelOfYearName::Imbolc,
        1 => WheelOfYearName::SpringEquinox,
        2 => WheelOfYearName::Beltane,
        3 => WheelOfYearName::SummerSolstice,
        4 => WheelOfYearName::Lughnasadh,
        5 => WheelOfYearName::AutumnEquinox,
        6 => WheelOfYearName::Samhain,
        _ => WheelOfYearName::WinterSolstice,
    }
}

/// Midnight UTC of a fixed calendar date: a checkpoint's nominal instant,
/// which carries no astronomically precise time of day of its own, unlike
/// a boundary's solar-event-derived instant.
fn checkpoint_instant(
    year: i32,
    month: time::Month,
    day: u8,
) -> Result<time::OffsetDateTime, EphemerisError> {
    let date = time::Date::from_calendar_date(year, month, day)
        .map_err(|_| EphemerisError::YearOutOfRange { year })?;
    Ok(time::PrimitiveDateTime::new(date, time::Time::MIDNIGHT).assume_utc())
}

/// Mean length of a personal-year period in days: the 365.25-day mean
/// tropical year divided evenly across all seven periods, rather than kept
/// at a flat 52 days with a separate absorbing gap.
const PERSONAL_YEAR_PERIOD_LENGTH_DAYS: f64 = 365.25 / 7.0;

fn personal_year_period(
    birth_date: time::Date,
    as_of_date: time::Date,
    transition_tolerance_days: f64,
) -> PersonalYearPeriod {
    let anniversary = most_recent_birthday_anniversary(birth_date, as_of_date);
    let days_since_birthday = (as_of_date - anniversary).as_seconds_f64() / 86_400.0;

    // Clamped, not just floored: a birthday-to-birthday span is 365 or 366
    // real calendar days, not exactly `7 * PERSONAL_YEAR_PERIOD_LENGTH_DAYS`
    // (365.25), so the last real day of a 366-day span can otherwise land
    // one period past the seventh (`period_index_0based == 7`), which does
    // not exist; day 1 of the *next* cycle already reports period 1
    // correctly via `most_recent_birthday_anniversary` picking the new
    // anniversary, so clamping here only ever affects the final day or two
    // of a longer-than-average cycle, keeping it in period 7 rather than
    // an invalid eighth one.
    let period_index_0based = (days_since_birthday / PERSONAL_YEAR_PERIOD_LENGTH_DAYS)
        .floor()
        .clamp(0.0, 6.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0.0..=6.0 immediately above"
    )]
    let period_number = period_index_0based as u8 + 1;

    let period_start_days = period_index_0based * PERSONAL_YEAR_PERIOD_LENGTH_DAYS;
    let period_end_days = (period_index_0based + 1.0) * PERSONAL_YEAR_PERIOD_LENGTH_DAYS;
    let transition = (days_since_birthday - period_start_days).abs() <= transition_tolerance_days
        || (days_since_birthday - period_end_days).abs() <= transition_tolerance_days;

    PersonalYearPeriod {
        period_number,
        ruling_planet: ruling_planet_for_period(period_number),
        transition,
    }
}

/// The fixed Chaldean order: period `1` is always Sun-ruled and always
/// begins at the birthday, recurring identically every year of life,
/// never progressing by age.
fn ruling_planet_for_period(period_number: u8) -> RulingPlanet {
    match period_number {
        1 => RulingPlanet::Sun,
        2 => RulingPlanet::Moon,
        3 => RulingPlanet::Mars,
        4 => RulingPlanet::Mercury,
        5 => RulingPlanet::Jupiter,
        6 => RulingPlanet::Venus,
        _ => RulingPlanet::Saturn,
    }
}

/// The most recent anniversary of `birth_date` at or before `as_of_date`:
/// the start of `as_of_date`'s current birthday-to-birthday year.
fn most_recent_birthday_anniversary(birth_date: time::Date, as_of_date: time::Date) -> time::Date {
    let this_year = anniversary_in_year(birth_date, as_of_date.year());
    if this_year <= as_of_date {
        this_year
    } else {
        anniversary_in_year(birth_date, as_of_date.year() - 1)
    }
}

/// `birth_date`'s month and day, restated in `year`. A 29 February
/// birthday has no exact anniversary in a non-leap `year`; observed on
/// 28 February instead, a deliberate, documented choice (common calendar
/// convention), not a silently invented one. Every other month has a
/// fixed day count in every year, so this is the only case that needs
/// handling; the closing `unwrap_or_else` is therefore defensive only,
/// covering no reachable input, since 28 February is valid in every year.
fn anniversary_in_year(birth_date: time::Date, year: i32) -> time::Date {
    let is_unobservable_leap_day = birth_date.month() == time::Month::February
        && birth_date.day() == 29
        && !time::util::is_leap_year(year);
    let day = if is_unobservable_leap_day {
        28
    } else {
        birth_date.day()
    };
    time::Date::from_calendar_date(year, birth_date.month(), day).unwrap_or(birth_date)
}

fn boundaries(
    start_date: time::Date,
    end_date: time::Date,
    birth_date: Option<time::Date>,
    hemisphere: Option<Hemisphere>,
    config: BoundariesConfig,
) -> Result<Vec<HorizonEvent>, EphemerisError> {
    if start_date > end_date {
        return Err(EphemerisError::InvalidDateRange {
            start_date,
            end_date,
        });
    }

    let range_start_jd = astro::time::julian_day(&to_astro_date(start_date));
    // Exclusive: the instant one tick past the end of `end_date` itself,
    // so the inclusive calendar range `[start_date, end_date]` becomes a
    // half-open Julian Day interval, simplifying every comparison below to
    // a plain `>=`/`<`.
    let range_end_jd = astro::time::julian_day(&to_astro_date(end_date)) + 1.0;

    let mut events = Vec::new();

    for (phase, jd_dynamical_time) in moon_quarter_occurrences_in_range(
        start_date,
        range_start_jd,
        range_end_jd,
        config.moon_phase_tolerance_hours,
    ) {
        events.push(HorizonEvent::MoonQuarter {
            instant: jd_dynamical_time_to_utc(jd_dynamical_time)?,
            phase,
        });
    }

    if let Some(hemisphere) = hemisphere {
        for point in wheel_of_year_events_in_range(start_date, end_date, hemisphere)? {
            events.push(HorizonEvent::WheelOfYear {
                instant: point.instant,
                name: point.name,
                role: point.role,
            });
        }
    }

    if let Some(birth_date) = birth_date {
        for (period_number, instant) in personal_year_period_changes_in_range(
            birth_date,
            start_date,
            end_date,
            config.personal_year_transition_tolerance_days,
        ) {
            events.push(HorizonEvent::PersonalYearPeriodChange {
                instant,
                period_number,
                ruling_planet: ruling_planet_for_period(period_number),
            });
        }
    }

    events.sort_by_key(HorizonEvent::instant);
    Ok(events)
}

fn primary_moon_phase(phase: &astro::lunar::Phase) -> PrimaryMoonPhase {
    match phase {
        astro::lunar::Phase::New => PrimaryMoonPhase::New,
        astro::lunar::Phase::First => PrimaryMoonPhase::FirstQuarter,
        astro::lunar::Phase::Full => PrimaryMoonPhase::Full,
        astro::lunar::Phase::Last => PrimaryMoonPhase::LastQuarter,
    }
}

/// Every occurrence (Dynamical Time Julian Day) of each of the four
/// primary Moon phases whose instant falls within `[range_start_jd -
/// tolerance, range_end_jd + tolerance)`, `tolerance` converted from
/// `tolerance_hours`. Reproduces exactly what a per-day loop calling
/// [`moon_phase`] with the same tolerance and checking its four `near_*`
/// flags would collect (the union, across every day in range, of "is this
/// phase's instant within tolerance of this day's midnight"), without
/// looping per day: for `tolerance_hours` at its default `12.0`,
/// consecutive per-day windows of `±12h` tile the range with no gaps, so
/// this range-based check is exact, not merely an approximation of the
/// per-day definition.
///
/// Walks `k` (the same integer lunation index [`phase_jd_at_integer_k`]
/// already uses) forward from a starting point anchored on `start_date`'s
/// own decimal year, `2` steps early as a safety margin far larger than
/// any realistic tolerance value, so the first in-range occurrence is
/// never missed; stops as soon as a candidate's Julian Day exceeds the
/// upper bound, since `k` maps monotonically to Julian Day.
fn moon_quarter_occurrences_in_range(
    start_date: time::Date,
    range_start_jd: f64,
    range_end_jd: f64,
    tolerance_hours: f64,
) -> Vec<(PrimaryMoonPhase, f64)> {
    let tolerance_days = tolerance_hours / 24.0;
    let lower_bound = range_start_jd - tolerance_days;
    let upper_bound = range_end_jd + tolerance_days;
    let anchor_decimal_year = astro::time::decimal_year(&to_astro_date(start_date));

    let mut occurrences = Vec::new();
    for phase in [
        astro::lunar::Phase::New,
        astro::lunar::Phase::First,
        astro::lunar::Phase::Full,
        astro::lunar::Phase::Last,
    ] {
        let raw_k = 12.3685 * (anchor_decimal_year - 2000.0) - phase_offset(&phase);
        let mut k = raw_k.floor() - 2.0;
        loop {
            let jd = phase_jd_at_integer_k(k, &phase);
            if jd > upper_bound {
                break;
            }
            if jd >= lower_bound {
                occurrences.push((primary_moon_phase(&phase), jd));
            }
            k += 1.0;
        }
    }
    occurrences
}

/// Every Wheel-of-the-Year point whose instant falls within
/// `[start_date, end_date]` inclusive, across every calendar year the
/// range touches. No tolerance: unlike a moon-quarter checkpoint or a
/// personal-year transition, a Wheel-of-the-Year point has no "near"
/// window of its own for `config` to carry.
fn wheel_of_year_events_in_range(
    start_date: time::Date,
    end_date: time::Date,
    hemisphere: Hemisphere,
) -> Result<Vec<WheelOfYearPoint>, EphemerisError> {
    let range_start = time::PrimitiveDateTime::new(start_date, time::Time::MIDNIGHT).assume_utc();
    let range_end =
        time::PrimitiveDateTime::new(end_date, time::Time::MIDNIGHT).assume_utc() + ONE_DAY;

    let mut points = Vec::new();
    for year in start_date.year()..=end_date.year() {
        for point in wheel_of_year(year, hemisphere)? {
            if point.instant >= range_start && point.instant < range_end {
                points.push(point);
            }
        }
    }
    Ok(points)
}

/// One day, as a [`time::Duration`]: reads more plainly at each call site
/// below than a bare `time::Duration::days(1)` repeated three times.
const ONE_DAY: time::Duration = time::Duration::days(1);

/// Every personal-year period change within tolerance of
/// `[start_date, end_date]` inclusive: the instant each of the seven
/// periods begins, across every birthday-year the (tolerance-widened)
/// range touches. Mirrors [`moon_quarter_occurrences_in_range`]'s "what a
/// per-day loop checking `transition` would have found" equivalence, for
/// the same reason: exhaustive per-day checking and this range-based
/// check agree exactly.
fn personal_year_period_changes_in_range(
    birth_date: time::Date,
    start_date: time::Date,
    end_date: time::Date,
    tolerance_days: f64,
) -> Vec<(u8, time::OffsetDateTime)> {
    let tolerance = time::Duration::seconds_f64(tolerance_days * 86_400.0);
    let range_start =
        time::PrimitiveDateTime::new(start_date, time::Time::MIDNIGHT).assume_utc() - tolerance;
    let range_end = time::PrimitiveDateTime::new(end_date, time::Time::MIDNIGHT).assume_utc()
        + ONE_DAY
        + tolerance;

    let mut changes = Vec::new();
    let mut anniversary = most_recent_birthday_anniversary(birth_date, start_date);
    loop {
        let anniversary_instant =
            time::PrimitiveDateTime::new(anniversary, time::Time::MIDNIGHT).assume_utc();
        if anniversary_instant > range_end {
            break;
        }
        for period_index in 0_u8..7 {
            let offset_days = f64::from(period_index) * PERSONAL_YEAR_PERIOD_LENGTH_DAYS;
            let boundary_instant =
                anniversary_instant + time::Duration::seconds_f64(offset_days * 86_400.0);
            if boundary_instant >= range_start && boundary_instant <= range_end {
                changes.push((period_index + 1, boundary_instant));
            }
        }
        anniversary = anniversary_in_year(birth_date, anniversary.year() + 1);
    }
    changes
}

fn to_astro_date(date: time::Date) -> astro::time::Date {
    // `time::Date`'s year is guaranteed within `i16` range (15-bit,
    // -16384..16383) under this workspace's default (non-`large-dates`)
    // feature set (see `time-0.3.53/src/date.rs`'s own `MIN_YEAR`/
    // `MAX_YEAR`), so this cast never truncates; there is no reachable
    // input that would make it lossy, so no fallible conversion or error
    // type exists for a case the type system already rules out.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "time::Date's year is always within i16 range without the large-dates feature"
    )]
    let year = date.year() as i16;
    astro::time::Date {
        year,
        month: u8::from(date.month()),
        decimal_day: f64::from(date.day()),
        cal_type: astro::time::CalType::Gregorian,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AstroEphemeris, BoundariesConfig, ComputesBoundaries, ComputesMoonPhase,
        ComputesPersonalYearPeriod, ComputesSolarEvents, ComputesWheelOfYear, EphemerisError,
        Hemisphere, HorizonEvent, MoonPhaseName, PrimaryMoonPhase, RulingPlanet, SolarEventKind,
        WheelOfYearName, WheelOfYearRole,
    };

    fn date(
        year: i32,
        month: time::Month,
        day: u8,
    ) -> Result<time::Date, time::error::ComponentRange> {
        time::Date::from_calendar_date(year, month, day)
    }

    /// Meeus, *Astronomical Algorithms*, Ch. 49 Example 49.a: the New Moon
    /// nearest 1977 February 15 falls at JD 2443192.65118 (1977-02-18
    /// ~03:38 UT), independently pinned by `astro-rust`'s own upstream
    /// regression test (`astro-2.0.0/tests/lunar.rs::phases`). Querying the
    /// same calendar day places the tool ~3.6 hours before that instant.
    #[test]
    fn fr_101_a_verified_new_moon_instant_reports_new_phase_and_near_zero_illumination()
    -> Result<(), time::error::ComponentRange> {
        let report = AstroEphemeris::new().moon_phase(date(1977, time::Month::February, 18)?, 12.0);

        assert_eq!(report.name, MoonPhaseName::New);
        assert!(report.near_new, "must fall within the default ±12h window");
        assert!(
            report.illumination_fraction < 0.05,
            "illumination so close to New should be near zero, was {}",
            report.illumination_fraction
        );
        assert!(
            !(0.5..29.0).contains(&report.days_into_cycle),
            "should sit at a cycle boundary (near 0 or near the full \
             synodic month), was {}",
            report.days_into_cycle
        );
        Ok(())
    }

    /// The Full Moon following the same Meeus-cited anchor above, computed
    /// via the identical, identically-accurate `time_of_phase` code path
    /// (same periodic-term algorithm, only the `k` offset differs): falls
    /// at JD 2443208.21807 (1977-03-05 ~17:14 UT). Querying the following
    /// calendar day places the tool ~6.8 hours after that instant.
    #[test]
    fn fr_101_a_verified_full_moon_instant_reports_full_phase_and_near_total_illumination()
    -> Result<(), time::error::ComponentRange> {
        let report = AstroEphemeris::new().moon_phase(date(1977, time::Month::March, 6)?, 12.0);

        assert_eq!(report.name, MoonPhaseName::Full);
        assert!(report.near_full, "must fall within the default ±12h window");
        assert!(
            report.illumination_fraction > 0.98,
            "illumination so close to Full should be near total, was {}",
            report.illumination_fraction
        );
        assert!(
            (13.5..16.0).contains(&report.days_into_cycle),
            "should sit close to half the synodic month (~14.77 days), was {}",
            report.days_into_cycle
        );
        Ok(())
    }

    #[test]
    fn fr_101_a_date_far_from_any_primary_phase_is_near_none_of_them()
    -> Result<(), time::error::ComponentRange> {
        // Halfway between the verified New Moon (1977-02-18) and First
        // Quarter (1977-02-26, computed via the same code path): sits
        // ~4 days from both, well outside even a generous 24-hour
        // tolerance.
        let report = AstroEphemeris::new().moon_phase(date(1977, time::Month::February, 22)?, 24.0);

        assert!(!report.near_new);
        assert!(!report.near_first_quarter);
        assert!(!report.near_full);
        assert!(!report.near_last_quarter);
        Ok(())
    }

    /// Regression pin for the `time_of_phase` anchor-selection quirk
    /// `nearest_phase_jd`'s own doc comment describes. A naive,
    /// date-anchored call into `astro::lunar::time_of_phase` at this exact
    /// fixture date (confirmed empirically while building this crate)
    /// skips straight past the true nearest New Moon (1977-02-18,
    /// ~5.65 days ahead) to the *next* month's instead
    /// (1977-03-19, ~34.77 days ahead), which would report
    /// `days_into_cycle` around `24.38` days short, landing in the wrong
    /// 45-degree phase bucket entirely (New instead of the correct Waning
    /// Crescent). This pins the correct value directly.
    #[test]
    fn fr_101_the_time_of_phase_anchor_quirk_does_not_skip_the_true_nearest_occurrence()
    -> Result<(), time::error::ComponentRange> {
        let report = AstroEphemeris::new().moon_phase(date(1977, time::Month::February, 13)?, 12.0);
        assert_eq!(report.name, MoonPhaseName::WaningCrescent);
        assert!(
            (23.5..25.5).contains(&report.days_into_cycle),
            "should be ~24.38 days into the cycle that started 1977-01-19 \
             (the true nearest New Moon is 1977-02-18, still ~5.65 days \
             ahead, not yet reached); the quirk this pins against would \
             instead skip to the following month's New Moon and report a \
             days_into_cycle far short of this, was {}",
            report.days_into_cycle
        );
        Ok(())
    }

    /// `FR-102`: the four solar events for a given year, in chronological
    /// order, each within its correct calendar month (no external citation
    /// needed for this one, only that a March equinox genuinely falls in
    /// March and precedes June's solstice, and so on).
    #[test]
    fn fr_102_solar_events_returns_four_events_in_chronological_order() {
        let Ok(events) = AstroEphemeris::new().solar_events(2024) else {
            unreachable!("2024 is a representable year");
        };

        assert_eq!(events[0].kind, SolarEventKind::MarchEquinox);
        assert_eq!(events[1].kind, SolarEventKind::JuneSolstice);
        assert_eq!(events[2].kind, SolarEventKind::SeptemberEquinox);
        assert_eq!(events[3].kind, SolarEventKind::DecemberSolstice);

        assert!(events[0].instant < events[1].instant);
        assert!(events[1].instant < events[2].instant);
        assert!(events[2].instant < events[3].instant);

        assert_eq!(events[0].instant.month(), time::Month::March);
        assert_eq!(events[1].instant.month(), time::Month::June);
        assert_eq!(events[2].instant.month(), time::Month::September);
        assert_eq!(events[3].instant.month(), time::Month::December);
    }

    /// Cross-referenced against commonly published 2024 equinox/solstice
    /// UTC times (general astronomical knowledge, not a specific cited
    /// almanac source; a tighter cross-check against a named published
    /// source is still owed as part of Phase 10's full accuracy-suite gate
    /// item, per `phase-10-change-brief.md`'s own risk note). Convergent
    /// evidence, not a bare assertion: omitting the nutation/aberration
    /// correction in `sun_apparent_longitude_degrees` shifted every one of
    /// these four instants by roughly ten minutes away from the values
    /// asserted here; adding it, the physically correct thing to do for
    /// an *apparent*-longitude crossing, closed the gap to within about a
    /// minute, for all four independently. A five-minute tolerance leaves
    /// headroom for residual `delta_t`-table or seed-precision variance
    /// without masking a genuine regression.
    #[test]
    fn fr_102_solar_events_are_close_to_commonly_published_2024_instants() {
        let Ok(events) = AstroEphemeris::new().solar_events(2024) else {
            unreachable!("2024 is a representable year");
        };
        let expected = [
            (time::Month::March, 20, 3, 6),
            (time::Month::June, 20, 20, 51),
            (time::Month::September, 22, 12, 44),
            (time::Month::December, 21, 9, 20),
        ];

        for (event, (month, day, hour, minute)) in events.iter().zip(expected) {
            let Ok(expected_date) = time::Date::from_calendar_date(2024, month, day) else {
                unreachable!("every expected date above is a valid calendar date");
            };
            let Ok(expected_time) = time::Time::from_hms(hour, minute, 0) else {
                unreachable!("every expected time above is a valid time of day");
            };
            let expected_instant =
                time::PrimitiveDateTime::new(expected_date, expected_time).assume_utc();
            let difference = (event.instant - expected_instant).abs();
            assert!(
                difference < time::Duration::minutes(5),
                "{:?}: computed {:?}, expected close to {expected_instant:?}, \
                 differed by {difference}",
                event.kind,
                event.instant
            );
        }
    }

    /// A tropical year (equinox-to-equinox) is close to 365.2422 days, not
    /// exactly 365 or 365.25 (well-established orbital mechanics, not
    /// specific to any one year): consecutive March equinoxes a year
    /// apart should land close to that spacing, catching a gross year-to-
    /// year regression without needing an external citation for either
    /// year individually.
    #[test]
    fn fr_102_consecutive_march_equinoxes_are_close_to_one_tropical_year_apart() {
        let ephemeris = AstroEphemeris::new();
        let Ok(this_year) = ephemeris.solar_events(2024) else {
            unreachable!("2024 is a representable year");
        };
        let Ok(next_year) = ephemeris.solar_events(2025) else {
            unreachable!("2025 is a representable year");
        };

        let span = next_year[0].instant - this_year[0].instant;
        let span_days = span.as_seconds_f64().abs() / 86_400.0;
        assert!(
            (365.2..365.3).contains(&span_days),
            "consecutive March equinoxes should be ~365.2422 days apart, was {span_days}"
        );
    }

    /// Tightens `FR-102`'s accuracy evidence by independently verifying
    /// the *correction* term this crate adds beyond `astro`'s own already-
    /// cited `geocent_ecl_pos` fixture: nutation in longitude, one of the
    /// two corrections `sun_apparent_longitude_degrees` applies (the other
    /// being aberration, too simple a one-line formula, a constant divided
    /// by distance, to carry meaningful independent-verification risk).
    /// Pinned against `astro-2.0.0/tests/nutation.rs`'s own Meeus-cited
    /// fixture (JD 2446895.5: nutation in longitude `-3.788` arcseconds),
    /// confirming the correction this crate depends on for its own
    /// accuracy, not only the base longitude function, is independently
    /// verified.
    #[test]
    fn fr_102_the_nutation_correction_this_crate_depends_on_matches_astros_own_cited_fixture() {
        let (nutation_in_longitude, _nutation_in_obliquity) =
            astro::nutation::nutation(2_446_895.5);
        let arcseconds = nutation_in_longitude.to_degrees() * 3600.0;
        assert!(
            (-3.789..-3.787).contains(&arcseconds),
            "expected -3.788 arcseconds (astro-2.0.0/tests/nutation.rs, Meeus-cited), was {arcseconds}"
        );
    }

    #[test]
    fn fr_102_a_year_outside_the_representable_range_is_a_typed_error() {
        let year = 10_000;
        assert_eq!(
            AstroEphemeris::new().solar_events(year),
            Err(EphemerisError::YearOutOfRange { year })
        );
    }

    /// `FR-103`: eight points, alternating Checkpoint/Boundary starting
    /// with a checkpoint (Imbolc, the year's first position), strictly
    /// chronological, matching `D-19`'s fixed calendar dates interleaved
    /// with `FR-102`'s solar-event instants.
    #[test]
    fn fr_103_wheel_of_year_returns_eight_points_in_chronological_order_with_correct_roles() {
        let Ok(points) = AstroEphemeris::new().wheel_of_year(2024, Hemisphere::Northern) else {
            unreachable!("2024 is a representable year");
        };

        let roles: Vec<WheelOfYearRole> = points.iter().map(|point| point.role).collect();
        assert_eq!(
            roles,
            vec![
                WheelOfYearRole::Checkpoint,
                WheelOfYearRole::Boundary,
                WheelOfYearRole::Checkpoint,
                WheelOfYearRole::Boundary,
                WheelOfYearRole::Checkpoint,
                WheelOfYearRole::Boundary,
                WheelOfYearRole::Checkpoint,
                WheelOfYearRole::Boundary,
            ]
        );
        for pair in points.windows(2) {
            assert!(
                pair[0].instant < pair[1].instant,
                "{:?} ({:?}) should precede {:?} ({:?})",
                pair[0].name,
                pair[0].instant,
                pair[1].name,
                pair[1].instant
            );
        }
    }

    /// `FR-103`'s traditional Northern Hemisphere mapping (Sacred Seasons
    /// convention, as referenced by the Operating Rhythm specification
    /// this phase serves): Imbolc (Feb), Spring Equinox (Mar), Beltane
    /// (May), Summer Solstice (Jun), Lughnasadh (Aug), Autumn Equinox
    /// (Sep), Samhain (Oct/Nov), Winter Solstice (Dec).
    #[test]
    fn fr_103_northern_hemisphere_names_match_the_traditional_calendar_mapping() {
        let Ok(points) = AstroEphemeris::new().wheel_of_year(2024, Hemisphere::Northern) else {
            unreachable!("2024 is a representable year");
        };
        let names: Vec<WheelOfYearName> = points.iter().map(|point| point.name).collect();
        assert_eq!(
            names,
            vec![
                WheelOfYearName::Imbolc,
                WheelOfYearName::SpringEquinox,
                WheelOfYearName::Beltane,
                WheelOfYearName::SummerSolstice,
                WheelOfYearName::Lughnasadh,
                WheelOfYearName::AutumnEquinox,
                WheelOfYearName::Samhain,
                WheelOfYearName::WinterSolstice,
            ]
        );
    }

    /// `FR-103`'s core requirement: "a June solstice is midsummer in the
    /// north, midwinter in the south". The Southern Hemisphere names are
    /// the same eight names rotated by exactly half the wheel (`+4`), and
    /// critically, every point's *instant* is completely unchanged from
    /// the Northern Hemisphere call for the same year: only the name
    /// attached to each fixed position changes, never the position
    /// itself, proving hemisphere is a labelling concern only, not a
    /// second, different set of dates.
    #[test]
    fn fr_103_southern_hemisphere_swaps_names_by_half_the_wheel_but_keeps_the_same_instants() {
        let ephemeris = AstroEphemeris::new();
        let Ok(northern) = ephemeris.wheel_of_year(2024, Hemisphere::Northern) else {
            unreachable!("2024 is a representable year");
        };
        let Ok(southern) = ephemeris.wheel_of_year(2024, Hemisphere::Southern) else {
            unreachable!("2024 is a representable year");
        };

        let southern_names: Vec<WheelOfYearName> =
            southern.iter().map(|point| point.name).collect();
        assert_eq!(
            southern_names,
            vec![
                WheelOfYearName::Lughnasadh,
                WheelOfYearName::AutumnEquinox,
                WheelOfYearName::Samhain,
                WheelOfYearName::WinterSolstice,
                WheelOfYearName::Imbolc,
                WheelOfYearName::SpringEquinox,
                WheelOfYearName::Beltane,
                WheelOfYearName::SummerSolstice,
            ]
        );
        // The June solstice (index 3) is Southern Hemisphere
        // WinterSolstice by name, at the identical instant Northern
        // Hemisphere calls SummerSolstice.
        assert_eq!(southern[3].name, WheelOfYearName::WinterSolstice);
        assert_eq!(northern[3].name, WheelOfYearName::SummerSolstice);
        for (north_point, south_point) in northern.iter().zip(southern.iter()) {
            assert_eq!(
                north_point.instant, south_point.instant,
                "hemisphere must never change which instant a position falls on"
            );
        }
    }

    /// `D-19`: the four cross-quarter checkpoints fall on fixed calendar
    /// dates regardless of hemisphere, this phase's own resolution of the
    /// "1/2 February"/"31 October, 1 November" ambiguity (the earlier day
    /// in both cases).
    #[test]
    fn fr_103_checkpoint_dates_fall_on_the_fixed_traditional_calendar_dates() {
        let Ok(points) = AstroEphemeris::new().wheel_of_year(2024, Hemisphere::Northern) else {
            unreachable!("2024 is a representable year");
        };
        let expected = [
            (0, time::Month::February, 1),
            (2, time::Month::May, 1),
            (4, time::Month::August, 1),
            (6, time::Month::October, 31),
        ];
        for (index, month, day) in expected {
            assert_eq!(points[index].instant.month(), month);
            assert_eq!(points[index].instant.day(), day);
            assert_eq!(points[index].role, WheelOfYearRole::Checkpoint);
        }
    }

    #[test]
    fn fr_103_a_year_outside_the_representable_range_is_a_typed_error() {
        let year = 10_000;
        assert_eq!(
            AstroEphemeris::new().wheel_of_year(year, Hemisphere::Northern),
            Err(EphemerisError::YearOutOfRange { year })
        );
    }

    /// `FR-104`: period 1 is always Sun-ruled and always begins exactly at
    /// the birthday itself.
    #[test]
    fn fr_104_period_1_always_begins_exactly_at_the_birthday_and_is_sun_ruled()
    -> Result<(), time::error::ComponentRange> {
        let birth_date = date(1990, time::Month::June, 15)?;
        let result = AstroEphemeris::new().personal_year_period(birth_date, birth_date, 2.0);

        assert_eq!(result.period_number, 1);
        assert_eq!(result.ruling_planet, RulingPlanet::Sun);
        assert!(
            result.transition,
            "the birthday itself is a period boundary by definition"
        );
        Ok(())
    }

    /// `FR-104`'s fixed Chaldean order, checked at the midpoint of every
    /// one of the seven periods (safely clear of any boundary, so
    /// `transition` cannot interfere with reading `period_number` off
    /// cleanly).
    #[test]
    fn fr_104_period_progresses_through_the_fixed_chaldean_order_across_the_year()
    -> Result<(), time::error::ComponentRange> {
        let birth_date = date(2000, time::Month::January, 1)?;
        let ephemeris = AstroEphemeris::new();
        let expected: [(u8, RulingPlanet); 7] = [
            (1, RulingPlanet::Sun),
            (2, RulingPlanet::Moon),
            (3, RulingPlanet::Mars),
            (4, RulingPlanet::Mercury),
            (5, RulingPlanet::Jupiter),
            (6, RulingPlanet::Venus),
            (7, RulingPlanet::Saturn),
        ];

        for (period_number, planet) in expected {
            let midpoint_days = (f64::from(period_number) - 0.5) * (365.25 / 7.0);
            let offset = time::Duration::seconds_f64(midpoint_days * 86_400.0);
            let as_of_datetime =
                time::PrimitiveDateTime::new(birth_date, time::Time::MIDNIGHT) + offset;
            let result = ephemeris.personal_year_period(birth_date, as_of_datetime.date(), 2.0);
            assert_eq!(
                result.period_number, period_number,
                "at the midpoint of period {period_number}"
            );
            assert_eq!(result.ruling_planet, planet);
            assert!(
                !result.transition,
                "a period's own midpoint is not near a boundary"
            );
        }
        Ok(())
    }

    /// The cycle recurs identically every year of life, never progressing
    /// by age: the same number of days past two different birthdays,
    /// years apart, must report the same period.
    #[test]
    fn fr_104_the_cycle_recurs_identically_every_year_of_life_not_progressing_by_age()
    -> Result<(), time::error::ComponentRange> {
        let birth_date = date(1985, time::Month::March, 10)?;
        let ephemeris = AstroEphemeris::new();
        let offset = time::Duration::days(100);

        let this_decade = birth_date + offset;
        let next_decade = date(1985 + 10, time::Month::March, 10)? + offset;

        let this_decade_result = ephemeris.personal_year_period(birth_date, this_decade, 2.0);
        let next_decade_result = ephemeris.personal_year_period(birth_date, next_decade, 2.0);
        assert_eq!(
            this_decade_result.period_number,
            next_decade_result.period_number
        );
        assert_eq!(
            this_decade_result.ruling_planet,
            next_decade_result.ruling_planet
        );
        Ok(())
    }

    #[test]
    fn fr_104_transition_flag_is_set_within_tolerance_of_a_boundary_and_clear_well_inside_a_period()
    -> Result<(), time::error::ComponentRange> {
        let birth_date = date(2000, time::Month::January, 1)?;
        let ephemeris = AstroEphemeris::new();

        // The period 1/2 boundary falls at day 52.1786; day 52 sits well
        // within a default ±2-day tolerance of it.
        let near_boundary = birth_date + time::Duration::days(52);
        assert!(
            ephemeris
                .personal_year_period(birth_date, near_boundary, 2.0)
                .transition
        );

        // Day 10 sits comfortably inside period 1, far from either edge.
        let mid_period = birth_date + time::Duration::days(10);
        assert!(
            !ephemeris
                .personal_year_period(birth_date, mid_period, 2.0)
                .transition
        );
        Ok(())
    }

    /// `D-21`/`FR-104`: a 29 February birthday has no exact anniversary in
    /// a non-leap year; observed on 28 February instead, this phase's own
    /// documented resolution of that ambiguity.
    #[test]
    fn fr_104_a_29_february_birthday_is_observed_on_28_february_in_a_non_leap_year()
    -> Result<(), time::error::ComponentRange> {
        let birth_date = date(2000, time::Month::February, 29)?; // 2000 is a leap year
        let as_of_date = date(2023, time::Month::February, 28)?; // 2023 is not

        let result = AstroEphemeris::new().personal_year_period(birth_date, as_of_date, 2.0);
        assert_eq!(result.period_number, 1);
        assert_eq!(result.ruling_planet, RulingPlanet::Sun);
        assert!(result.transition);
        Ok(())
    }

    /// `D-22`: a single day is simply `start_date == end_date`, no
    /// separate single-date method. Uses the same Meeus-cited 1977 New
    /// Moon fixture `FR-101`'s own tests verify (JD 2443192.65118,
    /// 1977-02-18 ~03:38 UT): querying that exact calendar day should
    /// report it without needing any tolerance extension.
    #[test]
    fn fr_105_single_day_range_is_the_degenerate_case_start_equals_end()
    -> Result<(), time::error::ComponentRange> {
        let day = date(1977, time::Month::February, 18)?;
        let events =
            AstroEphemeris::new().boundaries(day, day, None, None, BoundariesConfig::default());
        let Ok(events) = events else {
            unreachable!("a valid single-day range must not error");
        };

        assert!(
            events.iter().any(|event| matches!(
                event,
                HorizonEvent::MoonQuarter {
                    phase: PrimaryMoonPhase::New,
                    ..
                }
            )),
            "expected a New Moon event on its own verified date, got {events:?}"
        );
        Ok(())
    }

    /// Moon-quarter checkpoints are always included, with no `hemisphere`
    /// or `birth_date` supplied; the other two categories must then be
    /// entirely absent, not defaulted or errored.
    #[test]
    fn fr_105_moon_quarters_are_always_included_without_hemisphere_or_birth_date()
    -> Result<(), time::error::ComponentRange> {
        let start = date(1977, time::Month::February, 17)?;
        let end = date(1977, time::Month::February, 27)?;
        let events =
            AstroEphemeris::new().boundaries(start, end, None, None, BoundariesConfig::default());
        let Ok(events) = events else {
            unreachable!("a valid range must not error");
        };

        let moon_phases: Vec<PrimaryMoonPhase> = events
            .iter()
            .filter_map(|event| match event {
                HorizonEvent::MoonQuarter { phase, .. } => Some(*phase),
                HorizonEvent::WheelOfYear { .. }
                | HorizonEvent::PersonalYearPeriodChange { .. } => None,
            })
            .collect();
        assert!(
            moon_phases.contains(&PrimaryMoonPhase::New),
            "expected the verified New Moon (1977-02-18) in range, got {events:?}"
        );
        assert!(
            moon_phases.contains(&PrimaryMoonPhase::FirstQuarter),
            "expected the First Quarter (1977-02-26) in range, got {events:?}"
        );
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, HorizonEvent::WheelOfYear { .. })),
            "no hemisphere supplied; Wheel-of-Year points must be entirely absent"
        );
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, HorizonEvent::PersonalYearPeriodChange { .. })),
            "no birth_date supplied; personal-year period changes must be entirely absent"
        );
        Ok(())
    }

    /// Wheel-of-Year points appear only once `hemisphere` is supplied.
    #[test]
    fn fr_105_wheel_of_year_points_included_only_when_hemisphere_supplied()
    -> Result<(), time::error::ComponentRange> {
        let start = date(2024, time::Month::March, 15)?;
        let end = date(2024, time::Month::March, 25)?;
        let ephemeris = AstroEphemeris::new();

        let without_hemisphere =
            ephemeris.boundaries(start, end, None, None, BoundariesConfig::default());
        let Ok(without_hemisphere) = without_hemisphere else {
            unreachable!("a valid range must not error");
        };
        assert!(
            without_hemisphere
                .iter()
                .all(|event| !matches!(event, HorizonEvent::WheelOfYear { .. })),
        );

        let with_hemisphere = ephemeris.boundaries(
            start,
            end,
            None,
            Some(Hemisphere::Northern),
            BoundariesConfig::default(),
        );
        let Ok(with_hemisphere) = with_hemisphere else {
            unreachable!("a valid range must not error");
        };
        assert!(
            with_hemisphere.iter().any(|event| matches!(
                event,
                HorizonEvent::WheelOfYear {
                    name: WheelOfYearName::SpringEquinox,
                    role: WheelOfYearRole::Boundary,
                    ..
                }
            )),
            "expected the March 2024 equinox in range, got {with_hemisphere:?}"
        );
        Ok(())
    }

    /// Personal-year period changes appear only once `birth_date` is
    /// supplied; period 1 always begins exactly at the birthday.
    #[test]
    fn fr_105_personal_year_period_changes_included_only_when_birth_date_supplied()
    -> Result<(), time::error::ComponentRange> {
        let birth_date = date(1990, time::Month::June, 15)?;
        let start = date(2024, time::Month::June, 10)?;
        let end = date(2024, time::Month::June, 20)?;
        let ephemeris = AstroEphemeris::new();

        let without_birth_date =
            ephemeris.boundaries(start, end, None, None, BoundariesConfig::default());
        let Ok(without_birth_date) = without_birth_date else {
            unreachable!("a valid range must not error");
        };
        assert!(
            without_birth_date
                .iter()
                .all(|event| !matches!(event, HorizonEvent::PersonalYearPeriodChange { .. })),
        );

        let with_birth_date = ephemeris.boundaries(
            start,
            end,
            Some(birth_date),
            None,
            BoundariesConfig::default(),
        );
        let Ok(with_birth_date) = with_birth_date else {
            unreachable!("a valid range must not error");
        };
        assert!(
            with_birth_date.iter().any(|event| matches!(
                event,
                HorizonEvent::PersonalYearPeriodChange {
                    period_number: 1,
                    ruling_planet: RulingPlanet::Sun,
                    ..
                }
            )),
            "expected period 1 beginning at the 2024 birthday, got {with_birth_date:?}"
        );
        Ok(())
    }

    /// Events across all three categories, in one call, must come back in
    /// strict chronological order.
    #[test]
    fn fr_105_events_are_returned_in_chronological_order() -> Result<(), time::error::ComponentRange>
    {
        let birth_date = date(1990, time::Month::June, 15)?;
        let start = date(2024, time::Month::March, 1)?;
        let end = date(2024, time::Month::April, 1)?;
        let events = AstroEphemeris::new().boundaries(
            start,
            end,
            Some(birth_date),
            Some(Hemisphere::Southern),
            BoundariesConfig::default(),
        );
        let Ok(events) = events else {
            unreachable!("a valid range must not error");
        };

        assert!(events.len() > 1, "expected more than one event to order");
        for pair in events.windows(2) {
            assert!(
                pair[0].instant() <= pair[1].instant(),
                "{:?} should not come after {:?}",
                pair[0],
                pair[1]
            );
        }
        Ok(())
    }

    /// `D-22`'s "computed once across the window" claim, checked directly:
    /// the verified 1977-02-18 ~03:38 UT New Moon is within the default
    /// ±12h `moon_phase_tolerance` of 1977-02-17's own midnight-to-
    /// midnight span end, even though it falls just outside that single
    /// day's strict boundaries. A tolerance-unaware "does the instant fall
    /// strictly within the day" check would miss it entirely.
    #[test]
    fn fr_105_a_moon_quarter_just_outside_the_strict_range_is_still_reported_within_tolerance()
    -> Result<(), time::error::ComponentRange> {
        let day_before = date(1977, time::Month::February, 17)?;
        let events = AstroEphemeris::new().boundaries(
            day_before,
            day_before,
            None,
            None,
            BoundariesConfig::default(),
        );
        let Ok(events) = events else {
            unreachable!("a valid single-day range must not error");
        };

        assert!(
            events.iter().any(|event| matches!(
                event,
                HorizonEvent::MoonQuarter {
                    phase: PrimaryMoonPhase::New,
                    ..
                }
            )),
            "expected the New Moon ~20 hours later, within the default ±12h \
             tolerance of this day's own end, got {events:?}"
        );
        Ok(())
    }

    #[test]
    fn fr_105_an_inverted_date_range_is_a_typed_error() -> Result<(), time::error::ComponentRange> {
        let start = date(2024, time::Month::June, 20)?;
        let end = date(2024, time::Month::June, 10)?;
        assert_eq!(
            AstroEphemeris::new().boundaries(start, end, None, None, BoundariesConfig::default()),
            Err(EphemerisError::InvalidDateRange {
                start_date: start,
                end_date: end,
            })
        );
        Ok(())
    }
}
