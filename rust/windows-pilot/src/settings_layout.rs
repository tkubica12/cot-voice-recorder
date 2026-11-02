use crate::main_layout::Bounds;

pub(crate) const EDIT_FIELDS: [bool; 10] = [
    true, false, false, false, true, true, true, false, false, true,
];

pub(crate) struct SettingsLayout {
    pub fields: [Bounds; 10],
    pub labels: [Option<Bounds>; 10],
    pub hint: Bounds,
    pub save: Bounds,
    pub content_height: i32,
    pub viewport_height: i32,
}

impl SettingsLayout {
    pub fn calculate(width: i32, height: i32, dpi: u32) -> Result<Self, &'static str> {
        if !(48..=768).contains(&dpi) || width < 1 || height < 1 {
            return Err("Unsupported Settings viewport or DPI.");
        }
        let scale = |value: i32| (value * dpi as i32 + 48) / 96;
        if width < scale(200) {
            return Err("Settings viewport is too narrow to display its inputs.");
        }
        let wide = width >= scale(520);
        let mut fields = [Bounds::default(); 10];
        let mut labels = [None; 10];
        let left = scale(16);
        let content = width - left * 2;
        let mut top = scale(16);
        for (index, edit) in EDIT_FIELDS.into_iter().enumerate() {
            if edit {
                labels[index] = Some(Bounds {
                    x: left,
                    y: top,
                    width: if wide { scale(250) } else { content },
                    height: scale(30),
                });
                let x = if wide { scale(275) } else { left };
                fields[index] = Bounds {
                    x,
                    y: top + if wide { 0 } else { scale(32) },
                    width: width - left - x,
                    height: scale(26),
                };
                top += scale(if wide { 36 } else { 68 });
            } else {
                fields[index] = Bounds {
                    x: left,
                    y: top,
                    width: content,
                    height: scale(32),
                };
                top += scale(40);
            }
        }
        let hint = Bounds {
            x: left,
            y: top + scale(4),
            width: content,
            height: scale(if wide { 42 } else { 70 }),
        };
        let save = Bounds {
            x: left,
            y: hint.y + hint.height + scale(12),
            width: scale(145).min(content),
            height: scale(30),
        };
        Ok(Self {
            fields,
            labels,
            hint,
            save,
            content_height: save.y + save.height + scale(16),
            viewport_height: height,
        })
    }

    pub fn maximum_scroll(&self) -> i32 {
        (self.content_height - self.viewport_height).max(0)
    }

    pub fn clamp_scroll(&self, position: i32) -> i32 {
        position.clamp(0, self.maximum_scroll())
    }

    pub fn reveal(&self, position: i32, control: Bounds) -> i32 {
        let position = self.clamp_scroll(position);
        let bottom = control.y + control.height;
        self.clamp_scroll(if control.y < position {
            control.y
        } else if bottom > position + self.viewport_height {
            bottom - self.viewport_height
        } else {
            position
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_wide_and_high_dpi_inputs_remain_reachable_without_changing_content() {
        for dpi in [96, 120, 144, 192, 288] {
            for (width, height) in [(240, 160), (304, 210), (532, 500), (800, 700)] {
                let scale = |value: i32| (value * dpi as i32 + 48) / 96;
                let layout = SettingsLayout::calculate(scale(width), scale(height), dpi).unwrap();
                for control in layout
                    .fields
                    .into_iter()
                    .chain(layout.labels.into_iter().flatten())
                    .chain([layout.hint, layout.save])
                {
                    assert!(control.width > 0 && control.height > 0);
                    assert!(control.x >= 0 && control.x + control.width <= scale(width));
                    assert!(control.y >= 0 && control.y + control.height <= layout.content_height);
                }
                for control in layout.fields.into_iter().chain([layout.save]) {
                    for start in [0, layout.maximum_scroll(), i32::MAX, -1] {
                        let scroll = layout.reveal(start, control);
                        assert!(control.y - scroll >= 0);
                        assert!(control.y + control.height - scroll <= scale(height));
                    }
                }
                for (index, edit) in EDIT_FIELDS.into_iter().enumerate() {
                    assert_eq!(layout.labels[index].is_some(), edit);
                    if let Some(label) = layout.labels[index] {
                        let field = layout.fields[index];
                        assert!(
                            label.x + label.width <= field.x || label.y + label.height <= field.y
                        );
                    }
                }
            }
        }
        assert!(SettingsLayout::calculate(0, 100, 96).is_err());
        assert!(SettingsLayout::calculate(100, 100, 96).is_err());
        assert!(SettingsLayout::calculate(500, 100, 0).is_err());
    }
}
