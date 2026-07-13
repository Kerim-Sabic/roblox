use nectarpilot_core::transport::{MAX_FRAME_BYTES, NamedPipeSpec};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PipeListenerError {
    #[error("unsupported named-pipe protocol version {actual}; expected {expected}")]
    UnsupportedProtocol { actual: u16, expected: u16 },
    #[error("named-pipe path is not the expected versioned NectarPilot user path")]
    InvalidPath,
    #[error("named-pipe frame limit must be exactly {MAX_FRAME_BYTES} bytes")]
    InvalidFrameLimit,
    #[error("named pipe must reject remote clients")]
    RemoteClientsAllowed,
    #[error("named pipe must require a current-user-only ACL")]
    CurrentUserAclNotRequired,
    #[error("named-pipe operating-system operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("named-pipe security setup failed: {0}")]
    Security(String),
}

/// Rejects hand-built or downgraded specifications before any pipe object is
/// opened. The suffix is the 16-character lowercase SHA-256 user discriminator
/// produced by core's `NamedPipeSpec` constructor.
pub fn validate_pipe_spec(spec: &NamedPipeSpec) -> Result<(), PipeListenerError> {
    let supported = NamedPipeSpec::for_user_identity("validation-only").protocol_version;
    if spec.protocol_version != supported {
        return Err(PipeListenerError::UnsupportedProtocol {
            actual: spec.protocol_version,
            expected: supported,
        });
    }
    let prefix = format!(r"\\.\pipe\nectarpilot-v{supported}-");
    let suffix = spec
        .path
        .strip_prefix(&prefix)
        .ok_or(PipeListenerError::InvalidPath)?;
    if suffix.len() != 16
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(PipeListenerError::InvalidPath);
    }
    if spec.max_frame_bytes != MAX_FRAME_BYTES {
        return Err(PipeListenerError::InvalidFrameLimit);
    }
    if !spec.reject_remote_clients {
        return Err(PipeListenerError::RemoteClientsAllowed);
    }
    if !spec.current_user_acl_required {
        return Err(PipeListenerError::CurrentUserAclNotRequired);
    }
    Ok(())
}

#[cfg(windows)]
mod windows_listener {
    use std::ffi::c_void;
    use std::mem::size_of;

    use nectarpilot_core::transport::NamedPipeSpec;
    use tokio::net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions};
    use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE};
    use windows::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAce, GetLengthSid,
        GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor, PSECURITY_DESCRIPTOR,
        SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SetSecurityDescriptorDacl, TOKEN_QUERY,
        TOKEN_USER, TokenUser,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::BOOL;

    use super::{PipeListenerError, validate_pipe_spec};

    /// Tokio listener that always keeps the next secure pipe instance ready.
    pub struct SecureNamedPipeListener {
        spec: NamedPipeSpec,
        pending: Option<NamedPipeServer>,
    }

    impl SecureNamedPipeListener {
        pub fn bind(spec: NamedPipeSpec) -> Result<Self, PipeListenerError> {
            validate_pipe_spec(&spec)?;
            let pending = Some(create_instance(&spec, true)?);
            Ok(Self { spec, pending })
        }

        #[must_use]
        pub fn spec(&self) -> &NamedPipeSpec {
            &self.spec
        }

        /// Waits for one connection, creates the next secured instance before
        /// returning, and yields a full-duplex stream compatible with core's
        /// `CommandReceiver` and `EventSender`.
        pub async fn accept(&mut self) -> Result<NamedPipeServer, PipeListenerError> {
            let connected = self.pending.take().ok_or_else(|| {
                PipeListenerError::Io(std::io::Error::other(
                    "secure listener has no pending pipe instance",
                ))
            })?;
            connected.connect().await?;
            self.pending = Some(create_instance(&self.spec, false)?);
            Ok(connected)
        }
    }

    fn create_instance(
        spec: &NamedPipeSpec,
        first_instance: bool,
    ) -> Result<NamedPipeServer, PipeListenerError> {
        let mut security = CurrentUserSecurity::new()?;
        let mut options = ServerOptions::new();
        options
            .pipe_mode(PipeMode::Byte)
            .first_pipe_instance(first_instance)
            .reject_remote_clients(true)
            .max_instances(16)
            .in_buffer_size(64 * 1024)
            .out_buffer_size(64 * 1024);
        // SAFETY: CurrentUserSecurity owns a fully initialized SECURITY_ATTRIBUTES
        // graph for the entire synchronous CreateNamedPipeW call. Windows copies
        // the descriptor into the newly created kernel object before returning.
        unsafe {
            options.create_with_security_attributes_raw(&spec.path, security.attributes_ptr())
        }
        .map_err(PipeListenerError::Io)
    }

    struct CurrentUserSecurity {
        _acl_words: Vec<usize>,
        _descriptor: Box<SECURITY_DESCRIPTOR>,
        attributes: SECURITY_ATTRIBUTES,
    }

    impl CurrentUserSecurity {
        fn new() -> Result<Self, PipeListenerError> {
            let token = ProcessToken::current()?;
            let mut required = 0_u32;
            // SAFETY: the null-buffer query is the documented way to obtain the
            // required TOKEN_USER size; required is a valid writable out value.
            let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &raw mut required) };
            if required == 0 {
                return Err(PipeListenerError::Security(
                    windows::core::Error::from_win32().to_string(),
                ));
            }
            let mut token_words = aligned_words(required as usize);
            // SAFETY: token_words is aligned and writable for at least `required`
            // bytes and the token has TOKEN_QUERY rights.
            unsafe {
                GetTokenInformation(
                    token.0,
                    TokenUser,
                    Some(token_words.as_mut_ptr().cast::<c_void>()),
                    required,
                    &raw mut required,
                )
            }
            .map_err(|error| PipeListenerError::Security(error.to_string()))?;
            // SAFETY: successful TokenUser retrieval initialized TOKEN_USER at
            // the start of the aligned buffer for the buffer's lifetime.
            let user = unsafe { &*token_words.as_ptr().cast::<TOKEN_USER>() };
            // SAFETY: the SID belongs to the initialized TOKEN_USER structure.
            let sid_bytes = unsafe { GetLengthSid(user.User.Sid) } as usize;
            let acl_bytes = size_of::<ACL>()
                .checked_add(size_of::<ACCESS_ALLOWED_ACE>())
                .and_then(|size| size.checked_sub(size_of::<u32>()))
                .and_then(|size| size.checked_add(sid_bytes))
                .ok_or_else(|| PipeListenerError::Security("ACL size overflow".to_owned()))?;
            let acl_length = u32::try_from(acl_bytes)
                .map_err(|_| PipeListenerError::Security("ACL is too large".to_owned()))?;
            let mut acl_words = aligned_words(acl_bytes);
            let acl = acl_words.as_mut_ptr().cast::<ACL>();
            // SAFETY: acl points to aligned writable storage of acl_length bytes.
            unsafe { InitializeAcl(acl, acl_length, ACL_REVISION) }
                .map_err(|error| PipeListenerError::Security(error.to_string()))?;
            // SAFETY: AddAccessAllowedAce copies the valid current-user SID into
            // the initialized ACL; GENERIC_ALL is scoped to this pipe object.
            unsafe { AddAccessAllowedAce(acl, ACL_REVISION, GENERIC_ALL.0, user.User.Sid) }
                .map_err(|error| PipeListenerError::Security(error.to_string()))?;

            let mut descriptor = Box::<SECURITY_DESCRIPTOR>::default();
            let descriptor_pointer = PSECURITY_DESCRIPTOR((&raw mut *descriptor).cast::<c_void>());
            // SECURITY_DESCRIPTOR_REVISION is defined by Win32 as version 1.
            // SAFETY: descriptor_pointer addresses owned, aligned writable storage.
            unsafe { InitializeSecurityDescriptor(descriptor_pointer, 1) }
                .map_err(|error| PipeListenerError::Security(error.to_string()))?;
            // SAFETY: descriptor and ACL remain owned by CurrentUserSecurity until
            // after CreateNamedPipeW returns; the protected DACL has one user ACE.
            unsafe { SetSecurityDescriptorDacl(descriptor_pointer, true, Some(acl), false) }
                .map_err(|error| PipeListenerError::Security(error.to_string()))?;
            let attributes = SECURITY_ATTRIBUTES {
                nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
                    .expect("SECURITY_ATTRIBUTES size fits in u32"),
                lpSecurityDescriptor: descriptor_pointer.0,
                bInheritHandle: BOOL(0),
            };
            Ok(Self {
                _acl_words: acl_words,
                _descriptor: descriptor,
                attributes,
            })
        }

        fn attributes_ptr(&mut self) -> *mut c_void {
            (&raw mut self.attributes).cast::<c_void>()
        }
    }

    fn aligned_words(bytes: usize) -> Vec<usize> {
        let words = bytes.div_ceil(size_of::<usize>()).max(1);
        vec![0_usize; words]
    }

    struct ProcessToken(HANDLE);

    impl ProcessToken {
        fn current() -> Result<Self, PipeListenerError> {
            let mut token = HANDLE::default();
            // SAFETY: token is a valid writable out parameter. The pseudo-process
            // handle is always valid in the calling process.
            unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) }
                .map_err(|error| PipeListenerError::Security(error.to_string()))?;
            Ok(Self(token))
        }
    }

    impl Drop for ProcessToken {
        fn drop(&mut self) {
            // SAFETY: ProcessToken owns this handle exactly once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    pub use SecureNamedPipeListener as Listener;
}

#[cfg(windows)]
pub use windows_listener::Listener as SecureNamedPipeListener;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_core_generated_versioned_spec() {
        let spec = NamedPipeSpec::for_user_identity("DOMAIN\\Alice");
        validate_pipe_spec(&spec).unwrap();
    }

    #[test]
    fn rejects_version_and_unscoped_pipe_names() {
        let mut wrong_version = NamedPipeSpec::for_user_identity("DOMAIN\\Alice");
        wrong_version.protocol_version += 1;
        assert!(matches!(
            validate_pipe_spec(&wrong_version),
            Err(PipeListenerError::UnsupportedProtocol { .. })
        ));

        let mut unscoped = NamedPipeSpec::for_user_identity("DOMAIN\\Alice");
        unscoped.path = r"\\.\pipe\nectarpilot-v1-public".to_owned();
        assert!(matches!(
            validate_pipe_spec(&unscoped),
            Err(PipeListenerError::InvalidPath)
        ));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn secure_listener_exchanges_framed_data_with_a_same_user_client() {
        use std::time::{SystemTime, UNIX_EPOCH};

        use nectarpilot_contracts::{Command, CommandEnvelope, DaemonEvent, EventEnvelope};
        use nectarpilot_core::transport::{
            CommandReceiver, EventSender, connect_named_pipe, daemon_client,
        };
        use tokio::time::{Duration, timeout};
        use uuid::Uuid;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let spec =
            NamedPipeSpec::for_user_identity(&format!("test\\{}-{nonce}", std::process::id()));
        let mut listener = SecureNamedPipeListener::bind(spec.clone()).unwrap();
        let client = connect_named_pipe(&spec).await.unwrap();
        let server = timeout(Duration::from_secs(2), listener.accept())
            .await
            .expect("listener did not accept in time")
            .unwrap();

        let (mut command_sender, mut event_receiver) = daemon_client(client);
        let (server_reader, server_writer) = tokio::io::split(server);
        let mut command_receiver = CommandReceiver::new(server_reader);
        let mut event_sender = EventSender::new(server_writer);

        let command = CommandEnvelope::new(Uuid::nil(), Command::GetSnapshot);
        let second_command = CommandEnvelope::new(Uuid::nil(), Command::ExportProfile);
        command_sender.send(&command).await.expect("send command");
        command_sender
            .send(&second_command)
            .await
            .expect("send back-to-back command");
        assert_eq!(
            timeout(Duration::from_secs(2), command_receiver.next())
                .await
                .expect("command read timed out")
                .expect("command frame"),
            Some(command)
        );
        assert_eq!(
            timeout(Duration::from_secs(2), command_receiver.next())
                .await
                .expect("back-to-back command read timed out")
                .expect("back-to-back command frame"),
            Some(second_command)
        );

        let event = EventEnvelope::new(
            1,
            Uuid::nil(),
            DaemonEvent::CommandAccepted {
                request_id: Uuid::nil(),
            },
        );
        event_sender.send(&event).await.expect("send event");
        assert_eq!(
            timeout(Duration::from_secs(2), event_receiver.next())
                .await
                .expect("event read timed out")
                .expect("event frame"),
            Some(event)
        );
    }
}
