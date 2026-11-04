use coorworker_envd::ext::cua::LaunchContext;

#[test]
fn windows_service_and_unknown_sessions_cannot_initialize_desktop() {
    for (service_boot, session) in [
        (true, Some(1)), // Also applies to a service successor without an SCM handle.
        (true, None),
        (false, Some(0)),
        (false, None),
    ] {
        assert_eq!(
            LaunchContext::for_windows(service_boot, session),
            LaunchContext::ExternalOnly,
        );
    }
    assert_eq!(
        LaunchContext::for_windows(false, Some(1)),
        LaunchContext::DesktopSession,
    );
}
