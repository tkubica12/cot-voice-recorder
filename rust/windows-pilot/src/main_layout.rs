#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

pub(crate) const SECTION_ITEM_HEIGHT_DIP: i32 = 30;

pub(crate) struct MainLayout {
    pub logo: Bounds,
    pub title: Bounds,
    pub status: Bounds,
    pub status_card: Bounds,
    pub status_labels: [Bounds; 3],
    pub status_values: [Bounds; 3],
    pub toolbar: [Bounds; 5],
    pub tabs: Bounds,
    pub text_toggle: Bounds,
    pub history: Bounds,
    pub history_actions: [Bounds; 5],
    pub recovery_help: Bounds,
    pub recovery: Bounds,
    pub recovery_actions: [Bounds; 3],
}

fn flow<const N: usize>(
    widths: [i32; N],
    available: i32,
    scale: impl Fn(i32) -> i32,
) -> ([Bounds; N], i32) {
    let mut result = [Bounds::default(); N];
    let mut x = 0;
    let mut y = 0;
    for (index, width) in widths.into_iter().enumerate() {
        let width = scale(width);
        if x > 0 && x + width > available {
            x = 0;
            y += scale(38);
        }
        result[index] = Bounds {
            x,
            y,
            width,
            height: scale(30),
        };
        x += width + scale(8);
    }
    (result, y + scale(30))
}

impl MainLayout {
    pub fn calculate(width: i32, height: i32, dpi: u32) -> Result<Self, &'static str> {
        if !(48..=768).contains(&dpi) {
            return Err("Unsupported main-window DPI.");
        }
        let scale = |value: i32| (value * dpi as i32 + 48) / 96;
        if width < scale(528) || height < scale(340) {
            return Err("The main window is smaller than the supported reference viewport.");
        }
        let margin = scale(16);
        let content = width - margin * 2;
        let (mut toolbar, _) = flow([76, 76, 126, 88, 96], content, scale);
        for item in &mut toolbar {
            item.x += margin;
            item.y += scale(133);
        }
        let (mut history_actions, footer_height) = flow([110, 96, 108, 96, 108], content, scale);
        for item in &mut history_actions {
            item.x += margin;
            item.y += height - margin - footer_height;
        }
        let (mut recovery_actions, recovery_footer_height) = flow([130, 96, 130], content, scale);
        for item in &mut recovery_actions {
            item.x += margin;
            item.y += height - margin - recovery_footer_height;
        }
        let top = scale(219);
        let recovery_top = top + scale(56);
        let column_width = (content - scale(24)) / 3;
        let status_labels = std::array::from_fn(|index| Bounds {
            x: margin + scale(12) + column_width * index as i32,
            y: scale(78),
            width: column_width - scale(8),
            height: scale(16),
        });
        let status_values = std::array::from_fn(|index| Bounds {
            x: status_labels[index].x,
            y: scale(94),
            width: status_labels[index].width,
            height: scale(19),
        });
        Ok(Self {
            logo: Bounds {
                x: margin,
                y: scale(16),
                width: scale(40),
                height: scale(40),
            },
            title: Bounds {
                x: scale(68),
                y: scale(16),
                width: content - scale(52),
                height: scale(24),
            },
            status: Bounds {
                x: scale(68),
                y: scale(40),
                width: content - scale(52),
                height: scale(17),
            },
            status_card: Bounds {
                x: margin,
                y: scale(68),
                width: content,
                height: scale(53),
            },
            status_labels,
            status_values,
            toolbar,
            tabs: Bounds {
                x: margin,
                y: scale(175),
                width: content - scale(138),
                height: scale(32),
            },
            text_toggle: Bounds {
                x: width - margin - scale(130),
                y: scale(175),
                width: scale(130),
                height: scale(30),
            },
            history: Bounds {
                x: margin,
                y: top,
                width: content,
                height: height - margin - footer_height - scale(12) - top,
            },
            history_actions,
            recovery_help: Bounds {
                x: margin,
                y: top,
                width: content,
                height: scale(48),
            },
            recovery: Bounds {
                x: margin,
                y: recovery_top,
                width: content,
                height: height - margin - recovery_footer_height - scale(12) - recovery_top,
            },
            recovery_actions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn within(bounds: Bounds, width: i32, height: i32) {
        assert!(
            bounds.x >= 0 && bounds.y >= 0 && bounds.width > 0 && bounds.height > 0,
            "{bounds:?}"
        );
        assert!(
            bounds.x + bounds.width <= width && bounds.y + bounds.height <= height,
            "{bounds:?} in {width}x{height}"
        );
    }

    #[test]
    fn original_minimum_and_larger_viewports_fit_all_actions_at_every_dpi() {
        for dpi in [96, 120, 144, 192, 288] {
            for (logical_width, logical_height) in [(528, 340), (544, 381), (704, 501), (1000, 800)]
            {
                let width = logical_width * dpi / 96;
                let height = logical_height * dpi / 96;
                let layout = MainLayout::calculate(width as i32, height as i32, dpi).unwrap();
                for bounds in [
                    layout.status,
                    layout.logo,
                    layout.title,
                    layout.status_card,
                    layout.tabs,
                    layout.text_toggle,
                    layout.history,
                    layout.recovery_help,
                    layout.recovery,
                ]
                .into_iter()
                .chain(layout.toolbar)
                .chain(layout.status_labels)
                .chain(layout.status_values)
                .chain(layout.history_actions)
                .chain(layout.recovery_actions)
                {
                    within(bounds, width as i32, height as i32);
                }
                assert!(layout.history.y + layout.history.height < layout.history_actions[0].y);
                assert!(layout.tabs.height >= SECTION_ITEM_HEIGHT_DIP * dpi as i32 / 96);
                assert!(layout.tabs.x + layout.tabs.width < layout.text_toggle.x);
                assert!(
                    layout
                        .toolbar
                        .iter()
                        .all(|item| item.y == layout.toolbar[0].y)
                );
                assert!(layout.recovery_help.y + layout.recovery_help.height < layout.recovery.y);
                assert!(layout.recovery.y + layout.recovery.height < layout.recovery_actions[0].y);
            }
        }
    }

    #[test]
    fn narrow_footer_wraps_without_overlap_and_invalid_viewports_fail_explicitly() {
        let narrow = MainLayout::calculate(544, 381, 96).unwrap();
        let wide = MainLayout::calculate(704, 501, 96).unwrap();
        assert!(narrow.history_actions[4].y > narrow.history_actions[0].y);
        assert_eq!(wide.history_actions[4].y, wide.history_actions[0].y);
        assert!(MainLayout::calculate(100, 100, 96).is_err());
        assert!(MainLayout::calculate(704, 501, 0).is_err());
    }
}
