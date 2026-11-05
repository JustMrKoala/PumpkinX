use crate::{
    java::native_callbacks::CALLBACK_CONTEXT,
    proto::patchbukkit::{common::EmptyRequest, config::GetPatchBukkitConfigResponse},
};

pub fn ffi_native_bridge_get_patch_bukkit_config_impl(
    _request: EmptyRequest,
) -> Option<GetPatchBukkitConfigResponse> {
    CALLBACK_CONTEXT
        .get()
        .map(|context| GetPatchBukkitConfigResponse {
            minimum_supported_plugin_api: context
                .config
                .settings
                .minimum_supported_plugin_api
                .clone()
                .unwrap_or_else(|| "1.13".to_string()),
            disabled_plugins: context.config.plugins.disabled.clone(),
            download_dependencies: context.config.libraries.download_dependencies,
            warn_unimplemented_api: context.config.diagnostics.warn_unimplemented_api,
            debug_bridge: context.config.diagnostics.debug_bridge,
            log_level: context.config.diagnostics.log_level.clone(),
        })
}
