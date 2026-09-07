use rmcp::{
    Json, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};

use crate::{model::NotificationInput, service::NotificationService};

#[derive(Clone)]
pub struct NotificationMcp {
    service: NotificationService,
    tool_router: ToolRouter<Self>,
}

impl NotificationMcp {
    pub fn new(service: NotificationService) -> Self {
        Self {
            service,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl NotificationMcp {
    #[tool(
        name = "send_notification",
        description = "Send an authenticated notification to the owner's WeChat account"
    )]
    async fn send_notification(
        &self,
        Parameters(input): Parameters<NotificationInput>,
    ) -> Result<Json<crate::model::NotifyResponse>, String> {
        self.service
            .notify(input, "mcp")
            .await
            .map(Json)
            .map_err(|error| error.public_message())
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for NotificationMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "Use send_notification only when the user expects an external notification.",
            )
    }
}
