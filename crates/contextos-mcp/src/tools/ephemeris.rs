//! `ephemeris_moon_phase`, `ephemeris_solar_events`, `ephemeris_wheel_of_year`,
//! `ephemeris_personal_year_period`, `ephemeris_boundaries`: offline moon
//! phase, solar Wheel-of-the-Year, and personal-year period calculations
//! via `contextos_ephemeris`. Stateless
//! and read-only, the first tool family in this codebase with no
//! `VaultPath`, vault, or filesystem involvement at all. This module is
//! always compiled; only its tool router's *registration* into the
//! catalogue is gated behind the runtime `[server] astro` setting (or
//! `--astro` on the CLI), in `server.rs`.

use contextos_ephemeris::{
    AstroEphemeris, BoundariesConfig, ComputesBoundaries, ComputesMoonPhase, ComputesPersonalYearPeriod,
    ComputesSolarEvents, ComputesWheelOfYear, Hemisphere, HorizonEvent, MoonPhaseName, MoonPhaseReport,
    PersonalYearPeriod, PrimaryMoonPhase, RulingPlanet, SolarEvent, SolarEventKind, WheelOfYearName, WheelOfYearPoint,
    WheelOfYearRole,
};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{schemars, tool};
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;

use crate::resource_support::fallible_output_schema_for;
use crate::server::ContextOsServer;
use crate::tool_error::{ToolError, ToolFailure, execute};

#[rmcp::tool_router(router = ephemeris_tool_router, vis = "pub(crate)")]
impl ContextOsServer {
    #[tool(
        name = "ephemeris_moon_phase",
        description = "Compute the Moon's phase for a calendar date: its name (new, waxing_crescent, first_quarter, waxing_gibbous, full, waning_gibbous, last_quarter, waning_crescent), illumination fraction, days into the current synodic cycle, and whether the date falls within tolerance_hours (default 12) of each of the four primary phases' exact instant. Computed offline, no network access.",
        output_schema = fallible_output_schema_for::<MoonPhaseToolResult>()
    )]
    async fn ephemeris_moon_phase(
        &self,
        Parameters(input): Parameters<MoonPhaseInput>,
    ) -> Result<Json<MoonPhaseToolResult>, ToolFailure> {
        execute(move || {
            let date = parse_date(&input.date)?;
            let report = AstroEphemeris::new().moon_phase(date, input.tolerance_hours);
            Ok(MoonPhaseToolResult::from(report))
        })
        .await
    }

    #[tool(
        name = "ephemeris_solar_events",
        description = "Compute the exact UTC instant of both solstices and both equinoxes for a calendar year, in chronological order. Computed offline, no network access.",
        output_schema = fallible_output_schema_for::<SolarEventsToolResult>()
    )]
    async fn ephemeris_solar_events(
        &self,
        Parameters(input): Parameters<SolarEventsInput>,
    ) -> Result<Json<SolarEventsToolResult>, ToolFailure> {
        execute(move || {
            let events = AstroEphemeris::new()
                .solar_events(input.year)
                .map_err(ToolError::from)?;
            SolarEventsToolResult::try_from(events)
        })
        .await
    }

    #[tool(
        name = "ephemeris_wheel_of_year",
        description = "Compute all eight Wheel-of-the-Year points for a calendar year, in chronological order: the four solar events (role: boundary) and four cross-quarter days (role: checkpoint), each hemisphere-correctly named (a June solstice is midsummer in the north, midwinter in the south). Computed offline, no network access.",
        output_schema = fallible_output_schema_for::<WheelOfYearToolResult>()
    )]
    async fn ephemeris_wheel_of_year(
        &self,
        Parameters(input): Parameters<WheelOfYearInput>,
    ) -> Result<Json<WheelOfYearToolResult>, ToolFailure> {
        execute(move || {
            let points = AstroEphemeris::new()
                .wheel_of_year(input.year, input.hemisphere.into())
                .map_err(ToolError::from)?;
            WheelOfYearToolResult::try_from(points)
        })
        .await
    }

    #[tool(
        name = "ephemeris_personal_year_period",
        description = "Compute which of the seven annually-recurring personal-year periods contains as_of_date within the current birthday-to-birthday year, and that period's ruling planet in fixed Chaldean order (sun, moon, mars, mercury, jupiter, venus, saturn): period 1 always begins exactly at the birthday and the cycle recurs identically every year of life. transition is set when as_of_date falls within transition_tolerance_days (default 2) of a period boundary. No birth-time parameter; the source method carries none. Computed offline, no network access.",
        output_schema = fallible_output_schema_for::<PersonalYearPeriodToolResult>()
    )]
    async fn ephemeris_personal_year_period(
        &self,
        Parameters(input): Parameters<PersonalYearPeriodInput>,
    ) -> Result<Json<PersonalYearPeriodToolResult>, ToolFailure> {
        execute(move || {
            let birth_date = parse_date(&input.birth_date)?;
            let as_of_date = parse_date(&input.as_of_date)?;
            let ephemeris = AstroEphemeris::new();
            let period = ephemeris.personal_year_period(birth_date, as_of_date, input.transition_tolerance_days);
            Ok(PersonalYearPeriodToolResult::from(period))
        })
        .await
    }

    #[tool(
        name = "ephemeris_boundaries",
        description = "Aggregate moon phase, solar events, Wheel-of-the-Year, and personal-year period changes into every horizon boundary or checkpoint crossed within [start_date, end_date] inclusive, computed once across the window rather than once per day. A single day is simply start_date == end_date. Moon-quarter checkpoints are always included; Wheel-of-Year points are included only when hemisphere is supplied; personal-year period changes are included only when birth_date is supplied, so a caller with no operator-specific data still gets a valid, narrower result rather than an error. config optionally overrides the two named tolerance windows (moon_phase_tolerance_hours, default 12; personal_year_transition_tolerance_days, default 2).",
        output_schema = fallible_output_schema_for::<BoundariesToolResult>()
    )]
    async fn ephemeris_boundaries(
        &self,
        Parameters(input): Parameters<BoundariesInput>,
    ) -> Result<Json<BoundariesToolResult>, ToolFailure> {
        execute(move || {
            let start_date = parse_date(&input.start_date)?;
            let end_date = parse_date(&input.end_date)?;
            let birth_date = input.birth_date.as_deref().map(parse_date).transpose()?;
            let hemisphere = input.hemisphere.map(Hemisphere::from);
            let config = input
                .config
                .map_or_else(BoundariesConfig::default, BoundariesConfig::from);
            let events = AstroEphemeris::new()
                .boundaries(start_date, end_date, birth_date, hemisphere, config)
                .map_err(ToolError::from)?;
            let events = events
                .into_iter()
                .map(HorizonEventToolResult::try_from)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(BoundariesToolResult { events })
        })
        .await
    }
}

/// Parses a `YYYY-MM-DD` calendar date. Hand-rolled rather than via
/// `time`'s own parser (this crate does not enable the `parsing` feature,
/// matching every other date-handling call site here, which only ever
/// formats an internally generated date, never parses a caller-supplied
/// one; ephemeris is the first surface that does).
fn parse_date(raw: &str) -> Result<time::Date, ToolError> {
    const INVALID: ToolError = ToolError::Invalid("date must be in YYYY-MM-DD format");
    let mut parts = raw.split('-');
    let (Some(year_str), Some(month_str), Some(day_str), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(INVALID);
    };
    let year = year_str.parse::<i32>().map_err(|_| INVALID)?;
    let month = month_str.parse::<u8>().map_err(|_| INVALID)?;
    let day = day_str.parse::<u8>().map_err(|_| INVALID)?;
    let month = time::Month::try_from(month).map_err(|_| INVALID)?;
    time::Date::from_calendar_date(year, month, day).map_err(|_| INVALID)
}

/// Formats an instant as RFC 3339 (`2024-03-20T03:06:00Z`): unlike this
/// codebase's existing `OffsetDateTime::to_string()` call sites (a
/// human-readable filesystem-mtime hint, never parsed back), a caller of
/// these tools (the Operating Rhythm dispatcher skill) parses this value
/// programmatically, so a genuine, standard, unambiguous format matters
/// here in a way it does not for a debug-only field.
fn format_instant(instant: time::OffsetDateTime) -> Result<String, ToolError> {
    instant.format(&Rfc3339).map_err(ToolError::EphemerisInstantFormatting)
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MoonPhaseInput {
    /// Calendar date, `YYYY-MM-DD`.
    date: String,
    #[serde(default = "default_moon_phase_tolerance_hours")]
    tolerance_hours: f64,
}

const fn default_moon_phase_tolerance_hours() -> f64 {
    12.0
}

const fn default_personal_year_transition_tolerance_days() -> f64 {
    2.0
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SolarEventsInput {
    year: i32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct WheelOfYearInput {
    year: i32,
    hemisphere: HemisphereInput,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct PersonalYearPeriodInput {
    /// Calendar date, `YYYY-MM-DD`.
    birth_date: String,
    /// Calendar date, `YYYY-MM-DD`.
    as_of_date: String,
    #[serde(default = "default_personal_year_transition_tolerance_days")]
    transition_tolerance_days: f64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BoundariesInput {
    /// Calendar date, `YYYY-MM-DD`.
    start_date: String,
    /// Calendar date, `YYYY-MM-DD`.
    end_date: String,
    #[serde(default)]
    birth_date: Option<String>,
    #[serde(default)]
    hemisphere: Option<HemisphereInput>,
    #[serde(default)]
    config: Option<BoundariesConfigInput>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BoundariesConfigInput {
    #[serde(default = "default_moon_phase_tolerance_hours")]
    moon_phase_tolerance_hours: f64,
    #[serde(default = "default_personal_year_transition_tolerance_days")]
    personal_year_transition_tolerance_days: f64,
}

impl From<BoundariesConfigInput> for BoundariesConfig {
    fn from(value: BoundariesConfigInput) -> Self {
        Self {
            moon_phase_tolerance_hours: value.moon_phase_tolerance_hours,
            personal_year_transition_tolerance_days: value.personal_year_transition_tolerance_days,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum HemisphereInput {
    Northern,
    Southern,
}

impl From<HemisphereInput> for Hemisphere {
    fn from(value: HemisphereInput) -> Self {
        match value {
            HemisphereInput::Northern => Self::Northern,
            HemisphereInput::Southern => Self::Southern,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum MoonPhaseNameToolResult {
    New,
    WaxingCrescent,
    FirstQuarter,
    WaxingGibbous,
    Full,
    WaningGibbous,
    LastQuarter,
    WaningCrescent,
}

impl From<MoonPhaseName> for MoonPhaseNameToolResult {
    fn from(value: MoonPhaseName) -> Self {
        match value {
            MoonPhaseName::New => Self::New,
            MoonPhaseName::WaxingCrescent => Self::WaxingCrescent,
            MoonPhaseName::FirstQuarter => Self::FirstQuarter,
            MoonPhaseName::WaxingGibbous => Self::WaxingGibbous,
            MoonPhaseName::Full => Self::Full,
            MoonPhaseName::WaningGibbous => Self::WaningGibbous,
            MoonPhaseName::LastQuarter => Self::LastQuarter,
            MoonPhaseName::WaningCrescent => Self::WaningCrescent,
        }
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "the four near_* flags mirror MoonPhaseReport's own fields \
              one-for-one; not mutually exclusive, so an enum would misrepresent them"
)]
#[derive(Debug, Serialize, schemars::JsonSchema)]
struct MoonPhaseToolResult {
    name: MoonPhaseNameToolResult,
    illumination_fraction: f64,
    days_into_cycle: f64,
    near_new: bool,
    near_first_quarter: bool,
    near_full: bool,
    near_last_quarter: bool,
}

impl From<MoonPhaseReport> for MoonPhaseToolResult {
    fn from(value: MoonPhaseReport) -> Self {
        Self {
            name: value.name.into(),
            illumination_fraction: value.illumination_fraction,
            days_into_cycle: value.days_into_cycle,
            near_new: value.near_new,
            near_first_quarter: value.near_first_quarter,
            near_full: value.near_full,
            near_last_quarter: value.near_last_quarter,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SolarEventKindToolResult {
    MarchEquinox,
    JuneSolstice,
    SeptemberEquinox,
    DecemberSolstice,
}

impl From<SolarEventKind> for SolarEventKindToolResult {
    fn from(value: SolarEventKind) -> Self {
        match value {
            SolarEventKind::MarchEquinox => Self::MarchEquinox,
            SolarEventKind::JuneSolstice => Self::JuneSolstice,
            SolarEventKind::SeptemberEquinox => Self::SeptemberEquinox,
            SolarEventKind::DecemberSolstice => Self::DecemberSolstice,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct SolarEventToolResult {
    kind: SolarEventKindToolResult,
    /// RFC 3339 UTC instant.
    instant: String,
}

impl TryFrom<SolarEvent> for SolarEventToolResult {
    type Error = ToolError;

    fn try_from(value: SolarEvent) -> Result<Self, Self::Error> {
        Ok(Self {
            kind: value.kind.into(),
            instant: format_instant(value.instant)?,
        })
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct SolarEventsToolResult {
    events: Vec<SolarEventToolResult>,
}

impl TryFrom<[SolarEvent; 4]> for SolarEventsToolResult {
    type Error = ToolError;

    fn try_from(value: [SolarEvent; 4]) -> Result<Self, Self::Error> {
        Ok(Self {
            events: value
                .into_iter()
                .map(SolarEventToolResult::try_from)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum WheelOfYearNameToolResult {
    Imbolc,
    SpringEquinox,
    Beltane,
    SummerSolstice,
    Lughnasadh,
    AutumnEquinox,
    Samhain,
    WinterSolstice,
}

impl From<WheelOfYearName> for WheelOfYearNameToolResult {
    fn from(value: WheelOfYearName) -> Self {
        match value {
            WheelOfYearName::Imbolc => Self::Imbolc,
            WheelOfYearName::SpringEquinox => Self::SpringEquinox,
            WheelOfYearName::Beltane => Self::Beltane,
            WheelOfYearName::SummerSolstice => Self::SummerSolstice,
            WheelOfYearName::Lughnasadh => Self::Lughnasadh,
            WheelOfYearName::AutumnEquinox => Self::AutumnEquinox,
            WheelOfYearName::Samhain => Self::Samhain,
            WheelOfYearName::WinterSolstice => Self::WinterSolstice,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum WheelOfYearRoleToolResult {
    Boundary,
    Checkpoint,
}

impl From<WheelOfYearRole> for WheelOfYearRoleToolResult {
    fn from(value: WheelOfYearRole) -> Self {
        match value {
            WheelOfYearRole::Boundary => Self::Boundary,
            WheelOfYearRole::Checkpoint => Self::Checkpoint,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct WheelOfYearPointToolResult {
    name: WheelOfYearNameToolResult,
    role: WheelOfYearRoleToolResult,
    /// RFC 3339 UTC instant. For a boundary, the exact solar-event
    /// instant; for a checkpoint, midnight UTC of its fixed traditional
    /// calendar date.
    instant: String,
}

impl TryFrom<WheelOfYearPoint> for WheelOfYearPointToolResult {
    type Error = ToolError;

    fn try_from(value: WheelOfYearPoint) -> Result<Self, Self::Error> {
        Ok(Self {
            name: value.name.into(),
            role: value.role.into(),
            instant: format_instant(value.instant)?,
        })
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct WheelOfYearToolResult {
    points: Vec<WheelOfYearPointToolResult>,
}

impl TryFrom<[WheelOfYearPoint; 8]> for WheelOfYearToolResult {
    type Error = ToolError;

    fn try_from(value: [WheelOfYearPoint; 8]) -> Result<Self, Self::Error> {
        Ok(Self {
            points: value
                .into_iter()
                .map(WheelOfYearPointToolResult::try_from)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum RulingPlanetToolResult {
    Sun,
    Moon,
    Mars,
    Mercury,
    Jupiter,
    Venus,
    Saturn,
}

impl From<RulingPlanet> for RulingPlanetToolResult {
    fn from(value: RulingPlanet) -> Self {
        match value {
            RulingPlanet::Sun => Self::Sun,
            RulingPlanet::Moon => Self::Moon,
            RulingPlanet::Mars => Self::Mars,
            RulingPlanet::Mercury => Self::Mercury,
            RulingPlanet::Jupiter => Self::Jupiter,
            RulingPlanet::Venus => Self::Venus,
            RulingPlanet::Saturn => Self::Saturn,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct PersonalYearPeriodToolResult {
    period_number: u8,
    ruling_planet: RulingPlanetToolResult,
    transition: bool,
}

impl From<PersonalYearPeriod> for PersonalYearPeriodToolResult {
    fn from(value: PersonalYearPeriod) -> Self {
        Self {
            period_number: value.period_number,
            ruling_planet: value.ruling_planet.into(),
            transition: value.transition,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum PrimaryMoonPhaseToolResult {
    New,
    FirstQuarter,
    Full,
    LastQuarter,
}

impl From<PrimaryMoonPhase> for PrimaryMoonPhaseToolResult {
    fn from(value: PrimaryMoonPhase) -> Self {
        match value {
            PrimaryMoonPhase::New => Self::New,
            PrimaryMoonPhase::FirstQuarter => Self::FirstQuarter,
            PrimaryMoonPhase::Full => Self::Full,
            PrimaryMoonPhase::LastQuarter => Self::LastQuarter,
        }
    }
}

/// One aggregate boundary/checkpoint event, tagged by `kind`. Exactly one of
/// `phase`/(`name`, `role`)/(`period_number`, `ruling_planet`) is present,
/// matching which `kind` the event is; the flat, mostly-optional shape
/// (rather than a `oneOf`-composed schema) follows `resource_support.rs`'s
/// own `fallible_output_schema_for` precedent, where a `oneOf` output
/// schema was found live to make an entire connector's toolset disappear
/// inside a Cowork task.
#[derive(Debug, Serialize, schemars::JsonSchema)]
struct HorizonEventToolResult {
    kind: HorizonEventKindToolResult,
    /// RFC 3339 UTC instant.
    instant: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<PrimaryMoonPhaseToolResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<WheelOfYearNameToolResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<WheelOfYearRoleToolResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    period_number: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ruling_planet: Option<RulingPlanetToolResult>,
}

#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum HorizonEventKindToolResult {
    MoonQuarter,
    WheelOfYear,
    PersonalYearPeriodChange,
}

impl TryFrom<HorizonEvent> for HorizonEventToolResult {
    type Error = ToolError;

    fn try_from(value: HorizonEvent) -> Result<Self, Self::Error> {
        let instant = format_instant(value.instant())?;
        Ok(match value {
            HorizonEvent::MoonQuarter { phase, .. } => Self {
                kind: HorizonEventKindToolResult::MoonQuarter,
                instant,
                phase: Some(phase.into()),
                name: None,
                role: None,
                period_number: None,
                ruling_planet: None,
            },
            HorizonEvent::WheelOfYear { name, role, .. } => Self {
                kind: HorizonEventKindToolResult::WheelOfYear,
                instant,
                phase: None,
                name: Some(name.into()),
                role: Some(role.into()),
                period_number: None,
                ruling_planet: None,
            },
            HorizonEvent::PersonalYearPeriodChange {
                period_number,
                ruling_planet,
                ..
            } => Self {
                kind: HorizonEventKindToolResult::PersonalYearPeriodChange,
                instant,
                phase: None,
                name: None,
                role: None,
                period_number: Some(period_number),
                ruling_planet: Some(ruling_planet.into()),
            },
        })
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct BoundariesToolResult {
    events: Vec<HorizonEventToolResult>,
}
