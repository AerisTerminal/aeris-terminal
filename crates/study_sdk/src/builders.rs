//! Ergonomic fluent builders for authoring native Aeris studies without boilerplate.
//!
//! Builders produce the exact same validated runtime contracts (`StudyDefinition`,
//! `StudyOutputSpec`, `StudySettingSpec`) without hiding ownership, determinism, or bounds.

use crate::{
    BarSeriesKey, StreamRequirements, StudyColor, StudyDecimal, StudyDefinition, StudyDependency,
    StudyInvalidationPolicy, StudyMarketInput, StudyOutputId, StudyOutputSpec, StudyPaneTarget,
    StudyPlotKind, StudyPointStyle, StudyScaleTarget, StudySession, StudySettingChoiceOption,
    StudySettingCondition, StudySettingControl, StudySettingPresentation, StudySettingSpec,
    StudySettingValue, StudySource, StudySymbolRef, StudyThresholdRegion, StudyTimeframe,
};

/// Fluent builder for one [`StudyOutputSpec`].
#[derive(Clone, Debug)]
pub struct StudyOutputBuilder {
    spec: StudyOutputSpec,
}

impl StudyOutputBuilder {
    /// Starts a new scalar line output on the price pane.
    #[must_use]
    pub fn line(identifier: impl Into<String>, title: impl Into<String>) -> Self {
        Self::new(identifier, title, StudyPlotKind::Line)
    }

    /// Starts a new histogram output on dedicated pane group 1.
    #[must_use]
    pub fn histogram(identifier: impl Into<String>, title: impl Into<String>) -> Self {
        Self::new(identifier, title, StudyPlotKind::Histogram)
            .pane(StudyPaneTarget::Dedicated { group: 1 })
    }

    /// Starts a new output with an explicit [`StudyPlotKind`].
    #[must_use]
    pub fn new(
        identifier: impl Into<String>,
        title: impl Into<String>,
        plot: StudyPlotKind,
    ) -> Self {
        Self {
            spec: StudyOutputSpec {
                identifier: identifier.into(),
                title: title.into(),
                legend_label: None,
                plot,
                pane: StudyPaneTarget::Price,
                scale: StudyScaleTarget::Primary,
                threshold_region: None,
                point_style: StudyPointStyle::Uniform,
            },
        }
    }

    #[must_use]
    pub fn legend_label(mut self, label: impl Into<String>) -> Self {
        self.spec.legend_label = Some(label.into());
        self
    }

    #[must_use]
    pub const fn pane(mut self, pane: StudyPaneTarget) -> Self {
        self.spec.pane = pane;
        self
    }

    #[must_use]
    pub const fn scale(mut self, scale: StudyScaleTarget) -> Self {
        self.spec.scale = scale;
        self
    }

    #[must_use]
    pub const fn threshold_region(mut self, lower: StudyDecimal, upper: StudyDecimal) -> Self {
        self.spec.threshold_region = Some(StudyThresholdRegion { lower, upper });
        self
    }

    #[must_use]
    pub const fn point_style(mut self, style: StudyPointStyle) -> Self {
        self.spec.point_style = style;
        self
    }

    #[must_use]
    pub fn build(self) -> StudyOutputSpec {
        self.spec
    }
}

/// Fluent builder for one [`StudySettingSpec`].
#[derive(Clone, Debug)]
pub struct StudySettingBuilder {
    spec: StudySettingSpec,
}

impl StudySettingBuilder {
    #[must_use]
    pub fn boolean(identifier: impl Into<String>, default: bool) -> Self {
        Self {
            spec: StudySettingSpec::new(identifier, StudySettingValue::Boolean(default)),
        }
    }

    #[must_use]
    pub fn integer(identifier: impl Into<String>, default: i64) -> Self {
        Self {
            spec: StudySettingSpec::new(identifier, StudySettingValue::Integer(default)),
        }
    }

    #[must_use]
    pub fn decimal(identifier: impl Into<String>, default: StudyDecimal) -> Self {
        Self {
            spec: StudySettingSpec::new(identifier, StudySettingValue::Decimal(default)),
        }
    }

    #[must_use]
    pub fn text(identifier: impl Into<String>, default: impl Into<String>) -> Self {
        Self {
            spec: StudySettingSpec::new(identifier, StudySettingValue::Text(default.into())),
        }
    }

    #[must_use]
    pub fn choice(
        identifier: impl Into<String>,
        default: impl Into<String>,
        options: Vec<StudySettingChoiceOption>,
    ) -> Self {
        let identifier = identifier.into();
        let default_str = default.into();
        let mut spec =
            StudySettingSpec::new(identifier.clone(), StudySettingValue::Choice(default_str));
        spec.presentation = StudySettingPresentation {
            label: identifier,
            description: None,
            group: None,
            control: StudySettingControl::Choice { options },
            visible_when: None,
            enabled_when: None,
        };
        Self { spec }
    }

    #[must_use]
    pub fn color(identifier: impl Into<String>, default: StudyColor) -> Self {
        Self {
            spec: StudySettingSpec::new(identifier, StudySettingValue::Color(default)),
        }
    }

    #[must_use]
    pub fn symbol(
        identifier: impl Into<String>,
        default: StudySymbolRef,
        allow_current_chart: bool,
    ) -> Self {
        let mut builder = Self {
            spec: StudySettingSpec::new(identifier, StudySettingValue::Symbol(default)),
        };
        builder.spec.presentation.control = StudySettingControl::Symbol {
            allow_current_chart,
        };
        builder
    }

    #[must_use]
    pub fn timeframe(
        identifier: impl Into<String>,
        default: StudyTimeframe,
        allow_current_chart: bool,
    ) -> Self {
        let mut builder = Self {
            spec: StudySettingSpec::new(identifier, StudySettingValue::Timeframe(default)),
        };
        builder.spec.presentation.control = StudySettingControl::Timeframe {
            allow_current_chart,
        };
        builder
    }

    #[must_use]
    pub fn session(identifier: impl Into<String>, default: StudySession) -> Self {
        Self {
            spec: StudySettingSpec::new(identifier, StudySettingValue::Session(default)),
        }
    }

    #[must_use]
    pub fn timestamp(identifier: impl Into<String>, default_unix_nanos: i64) -> Self {
        Self {
            spec: StudySettingSpec::new(
                identifier,
                StudySettingValue::Timestamp(default_unix_nanos),
            ),
        }
    }

    #[must_use]
    pub fn price(identifier: impl Into<String>, default: StudyDecimal) -> Self {
        Self {
            spec: StudySettingSpec::new(identifier, StudySettingValue::Price(default)),
        }
    }

    #[must_use]
    pub fn source(
        identifier: impl Into<String>,
        default: StudySource,
        allow_study_output: bool,
    ) -> Self {
        let mut builder = Self {
            spec: StudySettingSpec::new(identifier, StudySettingValue::Source(default)),
        };
        builder.spec.presentation.control = StudySettingControl::Source { allow_study_output };
        builder
    }

    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.spec.presentation.label = label.into();
        self
    }

    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.spec.presentation.description = Some(description.into());
        self
    }

    #[must_use]
    pub fn group(mut self, group: impl Into<String>) -> Self {
        self.spec.presentation.group = Some(group.into());
        self
    }

    #[must_use]
    pub fn integer_bounds(
        mut self,
        minimum: Option<i64>,
        maximum: Option<i64>,
        step: Option<i64>,
    ) -> Self {
        if let StudySettingControl::Integer {
            minimum: min,
            maximum: max,
            step: st,
        } = &mut self.spec.presentation.control
        {
            *min = minimum;
            *max = maximum;
            *st = step;
        }
        self
    }

    #[must_use]
    pub fn visible_when(
        mut self,
        setting_identifier: impl Into<String>,
        equals: StudySettingValue,
    ) -> Self {
        self.spec.presentation.visible_when = Some(StudySettingCondition {
            setting_identifier: setting_identifier.into(),
            equals,
        });
        self
    }

    #[must_use]
    pub fn enabled_when(
        mut self,
        setting_identifier: impl Into<String>,
        equals: StudySettingValue,
    ) -> Self {
        self.spec.presentation.enabled_when = Some(StudySettingCondition {
            setting_identifier: setting_identifier.into(),
            equals,
        });
        self
    }

    #[must_use]
    pub fn build(self) -> StudySettingSpec {
        self.spec
    }
}

/// Fluent builder for one [`StudyDefinition`].
#[derive(Clone, Debug)]
pub struct StudyDefinitionBuilder {
    definition: StudyDefinition,
}

impl StudyDefinitionBuilder {
    /// Starts a new study definition with the given stable durable identifier.
    #[must_use]
    pub fn new(identifier: impl Into<String>) -> Self {
        Self {
            definition: StudyDefinition {
                identifier: identifier.into(),
                dependencies: Vec::new(),
                settings: Vec::new(),
                outputs: Vec::new(),
                invalidation: StudyInvalidationPolicy::SameRange,
            },
        }
    }

    /// Appends a canonical market-series dependency.
    #[must_use]
    pub fn market(mut self, series: BarSeriesKey, streams: StreamRequirements) -> Self {
        self.definition
            .dependencies
            .push(StudyDependency::Market(StudyMarketInput {
                series,
                streams,
            }));
        self
    }

    /// Appends a prior study-output dependency.
    #[must_use]
    pub fn output_dependency(mut self, output: StudyOutputId) -> Self {
        self.definition
            .dependencies
            .push(StudyDependency::Output(output));
        self
    }

    /// Appends one typed setting declaration.
    #[must_use]
    pub fn setting(mut self, spec: StudySettingSpec) -> Self {
        self.definition.settings.push(spec);
        self
    }

    /// Appends one scalar output declaration.
    #[must_use]
    pub fn output(mut self, spec: StudyOutputSpec) -> Self {
        self.definition.outputs.push(spec);
        self
    }

    /// Sets the invalidation policy for incremental recalculation.
    #[must_use]
    pub const fn invalidation(mut self, policy: StudyInvalidationPolicy) -> Self {
        self.definition.invalidation = policy;
        self
    }

    /// Finalizes the [`StudyDefinition`].
    #[must_use]
    pub fn build(self) -> StudyDefinition {
        self.definition
    }
}
