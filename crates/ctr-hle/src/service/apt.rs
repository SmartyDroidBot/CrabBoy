//! The applet manager, `APT:A`, `APT:S` and `APT:U` (3dbrew, "NS and APT
//! Services").
//!
//! An application starts by taking the APT lock handle, initialising with
//! two events (a notification event and a resume event), and enabling
//! itself; the HOME Menu then wakes it with a parameter, which it receives
//! after the resume event fires. Nothing else ever runs here: there is no
//! HOME Menu to return to, no applet to start, no sleep, so after the wake-up
//! the parameter queue stays empty and no notification comes.

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::kernel::{Handle, ObjId, Object, Reset};
use crate::result::{self, ResultCode};
use crate::service::Service;

/// `ReceiveParameter` with nothing to receive.
const NO_PARAMETER: ResultCode = ResultCode(0xC8A0_CFEF);
/// Applet ids (3dbrew, "NS and APT Services", "AppIDs").
const HOME_MENU: u32 = 0x101;
const APPLICATION: u32 = 0x300;
/// The command a wake-up parameter carries.
const WAKEUP: u32 = 1;

/// A message from one applet to another.
struct Parameter {
    sender: u32,
    command: u32,
    handle: Handle,
    data: Vec<u8>,
}

#[derive(Default)]
pub struct Apt {
    lock: Option<Handle>,
    /// The two events of `Initialize`: notification, then resume.
    events: Option<[ObjId; 2]>,
    parameter: Option<Parameter>,
    cpu_time_limit: u32,
    screen_capture_permission: u32,
}

impl Apt {
    /// The parameter the next `ReceiveParameter` gets, announced by the resume
    /// event.
    fn send(&mut self, h: &mut Horizon, parameter: Parameter) {
        self.parameter = Some(parameter);
        if let Some([_, resume]) = self.events {
            h.kernel.signal(resume);
        }
    }

    /// `ReceiveParameter` and `GlanceParameter`: the parameter, its data in
    /// the client's static buffer 0. Receiving it takes it away.
    fn parameter(&mut self, h: &mut Horizon, request: &Request, take: bool) -> Reply {
        let command = request.command();
        let Some(parameter) = (if take {
            self.parameter.take()
        } else {
            self.parameter.as_ref().map(|p| Parameter {
                sender: p.sender,
                command: p.command,
                handle: p.handle,
                data: p.data.clone(),
            })
        }) else {
            return Reply::new(command, NO_PARAMETER).words(&[0; 3]);
        };
        let (addr, room) = h.static_buffer(0);
        let len = parameter
            .data
            .len()
            .min(room as usize)
            .min(request.word(2) as usize);
        h.process
            .space
            .write_bytes(&mut h.mem, addr, &parameter.data[..len]);
        Reply::new(command, result::SUCCESS)
            .word(parameter.sender)
            .word(parameter.command)
            .word(len as u32)
            .handles(&[parameter.handle], true)
            .static_buffer(0, addr, len as u32)
    }
}

impl Service for Apt {
    fn name(&self) -> &str {
        "APT"
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        match command {
            // GetLockHandle(flags): attributes, state, the lock.
            0x01 => {
                let lock = *self.lock.get_or_insert_with(|| {
                    h.kernel.create_handle(Object::Mutex {
                        owner: None,
                        depth: 0,
                    })
                });
                ok().word(0).word(0).handles(&[lock], false)
            }
            // Initialize(app id, attributes): the notification and resume
            // events.
            0x02 => {
                let events = *self.events.get_or_insert_with(|| {
                    [(); 2].map(|_| {
                        h.kernel.create(Object::Event {
                            signalled: false,
                            reset: Reset::OneShot,
                        })
                    })
                });
                let handles = events.map(|id| h.kernel.open(id));
                ok().handles(&handles, false)
            }
            // Enable(attributes): the HOME Menu wakes the application.
            0x03 => {
                self.send(
                    h,
                    Parameter {
                        sender: HOME_MENU,
                        command: WAKEUP,
                        handle: 0,
                        data: Vec::new(),
                    },
                );
                ok()
            }
            // Finalize.
            0x04 => ok(),
            // GetAppletManInfo(position): position, requested applet, HOME
            // Menu, the applet in front.
            0x05 => ok().words(&[request.word(1), APPLICATION, HOME_MENU, APPLICATION]),
            // GetAppletInfo(app id): title, media, registered, loaded,
            // attributes.
            0x06 => {
                let title = h.program_id;
                ok().u64(title).words(&[0, 1, 1, 0])
            }
            // IsRegistered(app id): the application and the HOME Menu are.
            0x09 => {
                let id = request.word(1);
                ok().word((id == APPLICATION || id == HOME_MENU || id == 0x400) as u32)
            }
            // InquireNotification: none.
            0x0B => ok().word(0),
            // SendParameter: there is nobody to receive it.
            0x0C => ok(),
            0x0D => self.parameter(h, request, true),
            0x0E => self.parameter(h, request, false),
            // CancelParameter: whether one was cancelled.
            0x0F => {
                let had = self.parameter.take().is_some();
                ok().word(had as u32)
            }
            // ReplySleepQuery, ReplySleepNotificationComplete, NotifyToWait.
            0x3E | 0x3F | 0x43 => ok(),
            // AppletUtility(utility, input size, output size): its output is
            // zeros and its own result success.
            0x4B => {
                let (addr, room) = h.static_buffer(0);
                let len = request.word(3).min(room);
                h.process
                    .space
                    .write_bytes(&mut h.mem, addr, &vec![0; len as usize]);
                ok().word(0).static_buffer(0, addr, len)
            }
            // SetApplicationCpuTimeLimit(1, percent).
            0x4F => {
                self.cpu_time_limit = request.word(2);
                ok()
            }
            0x50 => ok().word(self.cpu_time_limit),
            // GetStartupArgument(size, type): none, an empty buffer.
            0x51 => {
                let (addr, room) = h.static_buffer(0);
                let len = request.word(1).min(room);
                h.process
                    .space
                    .write_bytes(&mut h.mem, addr, &vec![0; len as usize]);
                ok().word(0).static_buffer(0, addr, len)
            }
            0x55 => {
                self.screen_capture_permission = request.word(1);
                ok()
            }
            0x56 => ok().word(self.screen_capture_permission),
            // CheckNew3DSApp, CheckNew3DS: an Old 3DS.
            0x101 | 0x102 => ok().word(0),
            _ => h.unhandled("APT", request),
        }
    }
}
