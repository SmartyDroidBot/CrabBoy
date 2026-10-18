//! SpotPass, `boss:U` (3dbrew, "BOSS Services").
//!
//! BOSS runs background download tasks a title registers, keeping what
//! arrives (NS data) in its extra data. With no network nothing ever
//! arrives: tasks register and report that they have not run, the NS data
//! lists are empty, and the finish event never fires. What a title stores
//! (its storage, its opt-out flag) reads back as it left it.

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::kernel::ObjId;
use crate::result;
use crate::service::table::{event_handle, ok, to_mapped, words, Answer, Table};
use crate::service::Service;

static ANSWERS: &[Answer] = &[
    // InitializeSession(program id, process).
    ok(0x01),
    // RegisterPrivateRootCa, RegisterPrivateClientCert.
    ok(0x05),
    ok(0x06),
    // GetNewArrivalFlag: nothing new. RegisterNewArrivalEvent.
    words(0x07, &[0]),
    ok(0x08),
    // RegisterTask, UnregisterTask, ReconfigureTask, GetTaskIdList.
    ok(0x0B),
    ok(0x0C),
    ok(0x0D),
    ok(0x0E),
    // SendProperty, SendPropertyHandle.
    ok(0x14),
    ok(0x15),
    // UpdateTaskInterval, UpdateTaskCount, GetTaskInterval, GetTaskCount,
    // GetTaskServiceStatus.
    ok(0x17),
    ok(0x18),
    words(0x19, &[0]),
    words(0x1A, &[0]),
    words(0x1B, &[0]),
    // StartTask, StartTaskImmediate, CancelTask.
    ok(0x1C),
    ok(0x1D),
    ok(0x1E),
    // GetTaskState: state, a count, a flag; the task never ran.
    words(0x20, &[0, 0, 0]),
    // GetTaskResult, GetTaskCommErrorCode, GetTaskStatus, GetTaskError.
    words(0x21, &[0, 0, 0]),
    words(0x22, &[0, 0, 0, 0]),
    words(0x23, &[0]),
    words(0x24, &[0]),
    // DeleteNsData, SetNsDataAdditionalInfo, GetNsDataAdditionalInfo,
    // SetNsDataNewFlag, GetNsDataNewFlag, GetNsDataLastUpdate,
    // GetErrorCode.
    ok(0x26),
    ok(0x29),
    words(0x2A, &[0]),
    ok(0x2B),
    words(0x2C, &[0]),
    words(0x2D, &[0, 0]),
    words(0x2E, &[0]),
    // StartBgImmediate, GetTaskPriority, RegisterImmediateTask,
    // SetTaskQuery.
    ok(0x33),
    words(0x34, &[0]),
    ok(0x35),
    ok(0x36),
];

pub struct Boss {
    table: Table,
    /// SetStorageInfo's extra data id, size and media.
    storage: Option<(u64, u32, u32)>,
    /// RegisterStorageEntry's values.
    entry: Option<(u32, u16)>,
    storage_option: [u32; 3],
    optout: u32,
    finish_event: Option<ObjId>,
}

impl Default for Boss {
    fn default() -> Self {
        Boss {
            table: Table::new(ANSWERS),
            storage: None,
            entry: None,
            storage_option: [0; 3],
            optout: 0,
            finish_event: None,
        }
    }
}

impl Service for Boss {
    fn name(&self) -> &str {
        "boss:U"
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        // A mapped output buffer described at word `at`: zeros, as nothing
        // ever arrived.
        let empty = |h: &mut Horizon, reply: Reply, at: usize| {
            let descriptor = request.word(at);
            let zeros = vec![0; (descriptor >> 4) as usize];
            to_mapped(h, reply, descriptor, request.word(at + 1), &zeros)
        };
        match command {
            // SetStorageInfo(extra data id, size, media), UnregisterStorage,
            // GetStorageInfo.
            0x02 => {
                self.storage = Some((request.u64(1), request.word(3), request.word(4)));
                ok()
            }
            0x03 => {
                self.storage = None;
                self.entry = None;
                ok()
            }
            0x04 => ok().word(self.storage.map_or(0, |s| s.1)),
            // SetOptoutFlag, GetOptoutFlag.
            0x09 => {
                self.optout = request.word(1) & 0xFF;
                ok()
            }
            0x0A => ok().word(self.optout),
            // GetStepIdList, ReceiveProperty(id, size, buffer),
            // GetNsDataHeaderInfo, GetTaskInfo: nothing to say.
            0x0F => empty(h, ok(), 2),
            0x16 => empty(h, ok().word(0), 3),
            0x25 => empty(h, ok(), 3),
            0x27 => empty(h, ok(), 4),
            // GetNsDataIdList and its variants(filter, size, start id,
            // ...): no NS data, and the list is complete.
            0x10..=0x13 => empty(h, ok().words(&[0, 1]), 5),
            // GetTaskFinishHandle.
            0x1F => {
                let handle = event_handle(h, &mut self.finish_event);
                ok().handles(&[handle], false)
            }
            // ReadNsData(id, offset, size, buffer): nothing read.
            0x28 => empty(h, ok().words(&[0, 0]), 5),
            // RegisterStorageEntry(value, size, ...), GetStorageEntryInfo.
            0x2F => {
                self.entry = Some((request.word(1), request.word(2) as u16));
                ok()
            }
            0x30 => {
                let (value, size) = self.entry.unwrap_or((0, 0));
                ok().words(&[value, size as u32])
            }
            // SetStorageOption, GetStorageOption.
            0x31 => {
                self.storage_option = [request.word(1), request.word(2), request.word(3)];
                ok()
            }
            0x32 => ok().words(&self.storage_option),
            // GetTaskQuery(task id size, size, task id, buffer).
            0x37 => empty(h, ok(), 5),
            _ => match self.table.answer(h, request) {
                Some(reply) => reply,
                None => h.unhandled("boss:U", request),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::service::testing::{connect, console, event, send};

    #[test]
    fn storage_reads_back_and_the_finish_event_is_real() {
        let mut h = console();
        let boss = connect(&mut h, "boss:U");
        assert_eq!(
            send(&mut h, boss, &[0x0001_0082, 0, 0, 0x20, 0]),
            [0x0001_0040, 0]
        );
        assert_eq!(send(&mut h, boss, &[0x0030_0000]), [0x0030_00C0, 0, 0, 0]);
        send(&mut h, boss, &[0x002F_0140, 7, 0x40, 0, 0, 0]);
        assert_eq!(send(&mut h, boss, &[0x0030_0000])[2..], [7, 0x40]);
        let reply = send(&mut h, boss, &[0x001F_0000]);
        assert_eq!(event(&h, reply[3]), Some(false));
        assert!(h.log.is_empty(), "{:?}", h.log);
    }
}
