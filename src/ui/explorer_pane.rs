//! Retained render boundaries keep animated panes from rebuilding their siblings.

use gpui_kit::{Context, Entity, IntoElement, Render, Subscription, WeakEntity, Window, div};

use super::Explorer;

#[derive(Clone, Copy)]
pub(super) enum ExplorerPaneKind {
    Header,
    Topics,
    Details,
    Publish,
}

pub(super) struct ExplorerPane {
    explorer: WeakEntity<Explorer>,
    kind: ExplorerPaneKind,
    _subscription: Subscription,
    #[cfg(test)]
    pub(super) render_count: usize,
}

impl ExplorerPane {
    pub(super) fn new(explorer: &Entity<Explorer>, kind: ExplorerPaneKind, cx: &mut Context<Self>) -> Self {
        // Explicit shell/state changes invalidate all panes; child notifications
        // (animation, editor decorations, and clock ticks) do not emit this signal.
        let subscription = cx.observe(explorer, |_, _, cx| cx.notify());
        Self {
            explorer: explorer.downgrade(),
            kind,
            _subscription: subscription,
            #[cfg(test)]
            render_count: 0,
        }
    }
}

impl Render for ExplorerPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.render_count += 1;
        }
        let Some(explorer) = self.explorer.upgrade() else {
            return div().into_any_element();
        };
        explorer.update(cx, |explorer, cx| match self.kind {
            ExplorerPaneKind::Header => explorer.header(cx).into_any_element(),
            ExplorerPaneKind::Topics => explorer.topic_list(cx).into_any_element(),
            ExplorerPaneKind::Details => explorer.details(cx).into_any_element(),
            ExplorerPaneKind::Publish => explorer.publish_panel(cx).into_any_element(),
        })
    }
}
