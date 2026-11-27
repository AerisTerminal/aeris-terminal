//! Transactional, bounded semantic drawing scene and dashboard tables for native studies.
//!
//! Studies mutate a runtime-owned candidate `StudySceneState` using study-local
//! [`StudySceneObjectId`] handles. They never receive Aeris Charts or GPUI render
//! handles. On commit, the runtime computes compact `Create`/`Update`/`Delete`
//! [`StudySceneDelta`] lists; on error, panic, or deadline expiry, the candidate
//! is discarded and the last committed scene stays intact.

use super::{StudyColor, StudyLineStyle};
use std::collections::BTreeMap;

/// Maximum semantic scene objects owned by one study instance.
pub const MAXIMUM_STUDY_SCENE_OBJECTS: usize = 1_200;
/// Maximum vertices in one study polyline object.
pub const MAXIMUM_STUDY_POLYLINE_POINTS: usize = 512;
/// Maximum dashboard table objects owned by one study instance.
pub const MAXIMUM_STUDY_SCENE_TABLES: usize = 8;
/// Maximum populated cells in one study table object.
pub const MAXIMUM_STUDY_TABLE_CELLS: usize = 256;
/// Maximum UTF-8 bytes in one label, box, or table-cell text payload.
pub const MAXIMUM_STUDY_SCENE_TEXT_BYTES: usize = 256;

/// Stable study-local identity for one semantic scene object.
///
/// Independent of Aeris Charts object IDs and never persisted across restarts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StudySceneObjectId(pub u64);

/// Semantic chart anchor expressed in canonical exchange time and price.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StudyPoint {
    pub timestamp_unix_nanos: i64,
    pub price: f64,
}

impl StudyPoint {
    /// Creates one chart anchor.
    #[must_use]
    pub const fn new(timestamp_unix_nanos: i64, price: f64) -> Self {
        Self {
            timestamp_unix_nanos,
            price,
        }
    }

    fn validate(self) -> Result<(), String> {
        if !self.price.is_finite() {
            return Err("study scene point price must be finite".to_string());
        }
        Ok(())
    }
}

/// Ray/line extension semantics.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum StudyLineExtend {
    #[default]
    None,
    Right,
    Left,
    Both,
}

/// Semantic marker/label shape.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum StudyLabelStyle {
    #[default]
    LabelUp,
    LabelDown,
    LabelLeft,
    LabelRight,
    TextOnly,
    Circle,
    TriangleUp,
    TriangleDown,
    ArrowUp,
    ArrowDown,
    Cross,
}

/// Product-owned screen-space corner/edge placement for dashboard tables.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum StudyTablePosition {
    TopLeft,
    TopCenter,
    TopRight,
    MiddleLeft,
    MiddleCenter,
    MiddleRight,
    BottomLeft,
    BottomCenter,
    #[default]
    BottomRight,
}

/// Horizontal text alignment inside a table cell.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum StudyTextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// Semantic line or ray object.
#[derive(Clone, Debug, PartialEq)]
pub struct StudyLineObject {
    pub start: StudyPoint,
    pub end: StudyPoint,
    pub color: StudyColor,
    pub width: u8,
    pub style: StudyLineStyle,
    pub extend: StudyLineExtend,
}

/// Semantic rectangular zone/box object (e.g. FVG, order block, opening range).
#[derive(Clone, Debug, PartialEq)]
pub struct StudyBoxObject {
    pub top_left: StudyPoint,
    pub bottom_right: StudyPoint,
    pub border_color: StudyColor,
    pub border_width: u8,
    pub border_style: StudyLineStyle,
    pub fill_color: Option<StudyColor>,
    pub text: Option<String>,
    pub text_color: Option<StudyColor>,
}

/// Semantic multi-segment polyline or closed polygon object.
#[derive(Clone, Debug, PartialEq)]
pub struct StudyPolylineObject {
    pub points: Vec<StudyPoint>,
    pub color: StudyColor,
    pub width: u8,
    pub style: StudyLineStyle,
    pub fill_color: Option<StudyColor>,
    pub closed: bool,
}

/// Semantic shaded fill connecting two line objects in the same study scene.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StudyLineFillObject {
    pub first_line: StudySceneObjectId,
    pub second_line: StudySceneObjectId,
    pub color: StudyColor,
}

/// Semantic chart label or marker anchored at a time/price point.
#[derive(Clone, Debug, PartialEq)]
pub struct StudyLabelObject {
    pub point: StudyPoint,
    pub text: String,
    pub style: StudyLabelStyle,
    pub color: StudyColor,
    pub text_color: StudyColor,
}

/// One cell inside a study dashboard table.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StudyTableCell {
    pub text: String,
    pub text_color: Option<StudyColor>,
    pub background_color: Option<StudyColor>,
    pub align: StudyTextAlign,
}

/// Semantic dashboard table anchored to a product-owned viewport position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudyTableObject {
    pub position: StudyTablePosition,
    pub columns: u16,
    pub rows: u16,
    pub border_color: Option<StudyColor>,
    pub border_width: u8,
    pub cells: BTreeMap<(u16, u16), StudyTableCell>,
}

/// Union of all bounded semantic scene objects a study may own.
#[derive(Clone, Debug, PartialEq)]
pub enum StudySceneObject {
    Line(StudyLineObject),
    Box(StudyBoxObject),
    Polyline(StudyPolylineObject),
    LineFill(StudyLineFillObject),
    Label(StudyLabelObject),
    Table(StudyTableObject),
}

/// Compact incremental scene delta published after one committed study wave.
#[derive(Clone, Debug, PartialEq)]
pub enum StudySceneDelta {
    Create {
        id: StudySceneObjectId,
        object: StudySceneObject,
    },
    Update {
        id: StudySceneObjectId,
        object: StudySceneObject,
    },
    Delete {
        id: StudySceneObjectId,
    },
}

/// Runtime-owned transactional semantic scene for one study instance.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StudySceneState {
    objects: BTreeMap<StudySceneObjectId, StudySceneObject>,
    next_id: u64,
}

impl StudySceneState {
    /// Returns the number of live semantic objects in this scene.
    #[must_use]
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    /// Returns whether this scene has no live objects.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Looks up one object by its study-local identity.
    #[must_use]
    pub fn get(&self, id: StudySceneObjectId) -> Option<&StudySceneObject> {
        self.objects.get(&id)
    }

    /// Iterates all live objects in deterministic ID order.
    pub fn iter(&self) -> impl Iterator<Item = (StudySceneObjectId, &StudySceneObject)> {
        self.objects.iter().map(|(&id, obj)| (id, obj))
    }

    /// Allocates a new study-local identity and inserts a validated semantic object.
    ///
    /// # Errors
    /// Returns an error if scene object limits, table limits, or object invariants are violated.
    pub fn insert(&mut self, object: StudySceneObject) -> Result<StudySceneObjectId, String> {
        if self.objects.len() >= MAXIMUM_STUDY_SCENE_OBJECTS {
            return Err(format!(
                "study scene object count exceeds {MAXIMUM_STUDY_SCENE_OBJECTS}"
            ));
        }
        if matches!(object, StudySceneObject::Table(_))
            && self.table_count() >= MAXIMUM_STUDY_SCENE_TABLES
        {
            return Err(format!(
                "study scene table count exceeds {MAXIMUM_STUDY_SCENE_TABLES}"
            ));
        }
        self.validate_object(&object)?;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| "study scene object id exhausted".to_string())?;
        let id = StudySceneObjectId(self.next_id);
        self.objects.insert(id, object);
        Ok(id)
    }

    /// Creates a semantic line/ray object.
    ///
    /// # Errors
    /// Returns an error if bounds or coordinate validation fail.
    pub fn create_line(&mut self, line: StudyLineObject) -> Result<StudySceneObjectId, String> {
        self.insert(StudySceneObject::Line(line))
    }

    /// Creates a semantic box object.
    ///
    /// # Errors
    /// Returns an error if bounds or coordinate/text validation fail.
    pub fn create_box(&mut self, bx: StudyBoxObject) -> Result<StudySceneObjectId, String> {
        self.insert(StudySceneObject::Box(bx))
    }

    /// Creates a semantic polyline object.
    ///
    /// # Errors
    /// Returns an error if vertex limits or coordinate validation fail.
    pub fn create_polyline(
        &mut self,
        polyline: StudyPolylineObject,
    ) -> Result<StudySceneObjectId, String> {
        self.insert(StudySceneObject::Polyline(polyline))
    }

    /// Creates a semantic fill between two existing line objects.
    ///
    /// # Errors
    /// Returns an error if either line ID does not exist or is not a line.
    pub fn create_line_fill(
        &mut self,
        fill: StudyLineFillObject,
    ) -> Result<StudySceneObjectId, String> {
        self.insert(StudySceneObject::LineFill(fill))
    }

    /// Creates a semantic label/marker object.
    ///
    /// # Errors
    /// Returns an error if bounds, coordinate, or text validation fail.
    pub fn create_label(&mut self, label: StudyLabelObject) -> Result<StudySceneObjectId, String> {
        self.insert(StudySceneObject::Label(label))
    }

    /// Creates an empty bounded dashboard table object.
    ///
    /// # Errors
    /// Returns an error if table limits or dimensions are invalid.
    pub fn create_table(
        &mut self,
        position: StudyTablePosition,
        columns: u16,
        rows: u16,
        border_color: Option<StudyColor>,
        border_width: u8,
    ) -> Result<StudySceneObjectId, String> {
        self.insert(StudySceneObject::Table(StudyTableObject {
            position,
            columns,
            rows,
            border_color,
            border_width,
            cells: BTreeMap::new(),
        }))
    }

    /// Updates an existing semantic object in place.
    ///
    /// # Errors
    /// Returns an error if `id` does not exist or the replacement object is invalid.
    pub fn update(
        &mut self,
        id: StudySceneObjectId,
        object: StudySceneObject,
    ) -> Result<(), String> {
        let Some(existing) = self.objects.get(&id) else {
            return Err("study scene object does not exist".to_string());
        };
        if !matches!(existing, StudySceneObject::Table(_))
            && matches!(object, StudySceneObject::Table(_))
            && self.table_count() >= MAXIMUM_STUDY_SCENE_TABLES
        {
            return Err(format!(
                "study scene table count exceeds {MAXIMUM_STUDY_SCENE_TABLES}"
            ));
        }
        self.validate_object(&object)?;
        self.objects.insert(id, object);
        Ok(())
    }

    /// Writes one validated cell into an existing table object.
    ///
    /// # Errors
    /// Returns an error if `table_id` is not a table, coordinates are out of bounds, or cell limits fail.
    pub fn set_table_cell(
        &mut self,
        table_id: StudySceneObjectId,
        col: u16,
        row: u16,
        cell: StudyTableCell,
    ) -> Result<(), String> {
        if cell.text.len() > MAXIMUM_STUDY_SCENE_TEXT_BYTES {
            return Err(format!(
                "study table cell text exceeds {MAXIMUM_STUDY_SCENE_TEXT_BYTES} bytes"
            ));
        }
        let Some(StudySceneObject::Table(table)) = self.objects.get_mut(&table_id) else {
            return Err("study scene target is not a table".to_string());
        };
        if col >= table.columns || row >= table.rows {
            return Err("study table cell coordinates out of bounds".to_string());
        }
        if !table.cells.contains_key(&(col, row)) && table.cells.len() >= MAXIMUM_STUDY_TABLE_CELLS
        {
            return Err(format!(
                "study table cell count exceeds {MAXIMUM_STUDY_TABLE_CELLS}"
            ));
        }
        table.cells.insert((col, row), cell);
        Ok(())
    }

    /// Removes one object by ID. Returns `true` if the object existed.
    pub fn delete(&mut self, id: StudySceneObjectId) -> bool {
        self.objects.remove(&id).is_some()
    }

    /// Removes all objects from this scene while keeping the monotonic ID counter.
    pub fn clear(&mut self) {
        self.objects.clear();
    }

    /// Computes minimal compact `Create`/`Update`/`Delete` deltas from `self` to `next`.
    #[must_use]
    pub fn diff(&self, next: &Self) -> Vec<StudySceneDelta> {
        let mut deltas = Vec::new();
        for &id in self.objects.keys() {
            if !next.objects.contains_key(&id) {
                deltas.push(StudySceneDelta::Delete { id });
            }
        }
        for (&id, next_obj) in &next.objects {
            match self.objects.get(&id) {
                None => deltas.push(StudySceneDelta::Create {
                    id,
                    object: next_obj.clone(),
                }),
                Some(prev_obj) if prev_obj != next_obj => deltas.push(StudySceneDelta::Update {
                    id,
                    object: next_obj.clone(),
                }),
                Some(_) => {}
            }
        }
        deltas
    }

    fn table_count(&self) -> usize {
        self.objects
            .values()
            .filter(|object| matches!(object, StudySceneObject::Table(_)))
            .count()
    }

    fn validate_object(&self, object: &StudySceneObject) -> Result<(), String> {
        match object {
            StudySceneObject::Line(line) => {
                line.start.validate()?;
                line.end.validate()?;
                Ok(())
            }
            StudySceneObject::Box(bx) => {
                bx.top_left.validate()?;
                bx.bottom_right.validate()?;
                if bx
                    .text
                    .as_ref()
                    .is_some_and(|text| text.len() > MAXIMUM_STUDY_SCENE_TEXT_BYTES)
                {
                    return Err(format!(
                        "study box text exceeds {MAXIMUM_STUDY_SCENE_TEXT_BYTES} bytes"
                    ));
                }
                Ok(())
            }
            StudySceneObject::Polyline(poly) => {
                if poly.points.len() < 2 || poly.points.len() > MAXIMUM_STUDY_POLYLINE_POINTS {
                    return Err(format!(
                        "study polyline point count must be in 2..={MAXIMUM_STUDY_POLYLINE_POINTS}"
                    ));
                }
                for point in &poly.points {
                    point.validate()?;
                }
                Ok(())
            }
            StudySceneObject::LineFill(fill) => {
                if fill.first_line == fill.second_line {
                    return Err("study line fill requires two distinct lines".to_string());
                }
                if !matches!(
                    self.objects.get(&fill.first_line),
                    Some(StudySceneObject::Line(_))
                ) || !matches!(
                    self.objects.get(&fill.second_line),
                    Some(StudySceneObject::Line(_))
                ) {
                    return Err(
                        "study line fill references non-existent or non-line objects".to_string(),
                    );
                }
                Ok(())
            }
            StudySceneObject::Label(label) => {
                label.point.validate()?;
                if label.text.len() > MAXIMUM_STUDY_SCENE_TEXT_BYTES {
                    return Err(format!(
                        "study label text exceeds {MAXIMUM_STUDY_SCENE_TEXT_BYTES} bytes"
                    ));
                }
                Ok(())
            }
            StudySceneObject::Table(table) => {
                if table.columns == 0 || table.rows == 0 {
                    return Err("study table dimensions must be non-zero".to_string());
                }
                if table.cells.len() > MAXIMUM_STUDY_TABLE_CELLS {
                    return Err(format!(
                        "study table cell count exceeds {MAXIMUM_STUDY_TABLE_CELLS}"
                    ));
                }
                for (&(col, row), cell) in &table.cells {
                    if col >= table.columns || row >= table.rows {
                        return Err("study table cell coordinates out of bounds".to_string());
                    }
                    if cell.text.len() > MAXIMUM_STUDY_SCENE_TEXT_BYTES {
                        return Err(format!(
                            "study table cell text exceeds {MAXIMUM_STUDY_SCENE_TEXT_BYTES} bytes"
                        ));
                    }
                }
                Ok(())
            }
        }
    }
}
