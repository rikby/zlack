use serde::Serialize;
use tauri::Manager;

use crate::{
    security,
    windowing::{self, Document, PageEvent},
};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NotificationActivation {
    notification_id: String,
    team_id: Option<String>,
    channel_id: Option<String>,
}

impl NotificationActivation {
    pub(crate) fn new(
        notification_id: String,
        team_id: Option<String>,
        channel_id: Option<String>,
    ) -> Self {
        Self {
            notification_id,
            team_id: usable_context_value(team_id, security::valid_team),
            channel_id: usable_context_value(channel_id, security::valid_channel),
        }
    }

    pub(crate) fn target_url(&self) -> Option<String> {
        self.team_id.as_ref().map(|team_id| match &self.channel_id {
            Some(channel_id) => {
                format!("https://app.slack.com/client/{team_id}/{channel_id}")
            }
            None => format!("https://app.slack.com/client/{team_id}"),
        })
    }

    pub(crate) fn notification_id(&self) -> &str {
        &self.notification_id
    }

    pub(crate) fn update_context(&mut self, team_id: Option<String>, channel_id: Option<String>) {
        self.team_id = usable_context_value(team_id, security::valid_team);
        self.channel_id = usable_context_value(channel_id, security::valid_channel);
    }
}

fn usable_context_value(value: Option<String>, valid: fn(&str) -> bool) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        valid(value).then(|| value.to_string())
    })
}

pub(crate) fn activate_native_notification(
    app: &tauri::AppHandle,
    origin_label: &str,
    document: Option<Document>,
    activation: NotificationActivation,
) {
    if let (Some(window), Some(document)) = (app.get_webview_window(origin_label), document) {
        if windowing::document_is_current(&window, &document) {
            if let Some(window) = crate::focus_workspace_label(app, origin_label) {
                let target = activation
                    .target_url()
                    .and_then(|url| url::Url::parse(&url).ok());
                windowing::send_document_event(
                    &window,
                    document,
                    PageEvent::Notification(activation),
                    target,
                );
                return;
            }
        }
    }

    if let Some(team_id) = activation.team_id.as_deref() {
        crate::switch_to_workspace(app, team_id, activation.target_url());
    } else if let Some(window) = crate::active_window(app) {
        crate::restore_window(&window);
    }
}

#[cfg(test)]
mod tests {
    use super::NotificationActivation;

    #[test]
    fn builds_channel_fallback_only_with_valid_context() {
        let channel = NotificationActivation::new(
            "notification-1".to_string(),
            Some("T0123456".to_string()),
            Some("C0123456".to_string()),
        );
        let workspace = NotificationActivation::new(
            "notification-2".to_string(),
            Some("T0123456".to_string()),
            None,
        );
        let unknown = NotificationActivation::new("notification-3".to_string(), None, None);

        assert_eq!(
            channel.target_url().as_deref(),
            Some("https://app.slack.com/client/T0123456/C0123456")
        );
        assert_eq!(
            workspace.target_url().as_deref(),
            Some("https://app.slack.com/client/T0123456")
        );
        assert_eq!(unknown.target_url(), None);
    }

    #[test]
    fn ignores_invalid_context_values() {
        let activation = NotificationActivation::new(
            "notification-2".to_string(),
            Some("T0123456".to_string()),
            Some("C0123456/other".to_string()),
        );
        assert_eq!(
            activation.target_url().as_deref(),
            Some("https://app.slack.com/client/T0123456"),
        );
    }

    #[test]
    fn updates_a_pending_activation_with_late_context() {
        let mut activation = NotificationActivation::new("notification-1".to_string(), None, None);

        activation.update_context(Some("T0123456".to_string()), Some("D0123456".to_string()));

        assert_eq!(
            activation.target_url().as_deref(),
            Some("https://app.slack.com/client/T0123456/D0123456")
        );
    }
}
