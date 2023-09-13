// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020-2023 Stacks Open Internet Foundation
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

#![allow(unused_imports)]
#![allow(dead_code)]

use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::events::{EventReceiver, EventStopSignaler, StackerDBChunksEvent};

use crate::error::EventError;

use stacks_common::deps_common::ctrlc as termination;
use stacks_common::deps_common::ctrlc::SignalId;

use libc;

/// Some libcs, like musl, have a very small stack size.
/// Make sure it's big enough.
const THREAD_STACK_SIZE: usize = 128 * 1024 * 1024; // 128 MB

/// stderr fileno
const STDERR: i32 = 2;

/// Trait describing the needful components of a top-level runloop.
/// This is where the signer business logic would go.
/// Implement this, and you get all the multithreaded setup for free.
pub trait SignerRunLoop<R, CMD: Send> {
    /// Hint to set how long to wait for new events
    fn set_event_timeout(&mut self, timeout: Duration);
    /// Getter for the event poll timeout
    fn get_event_timeout(&self) -> Duration;
    /// Run one pass of the event loop, given new StackerDB events discovered since the last pass.
    /// Returns Some(R) if this is the final pass -- the runloop evaluated to R
    /// Returns None to keep running.
    fn run_one_pass(&mut self, event: Option<StackerDBChunksEvent>, cmd: Option<CMD>) -> Option<R>;

    /// This is the main loop body for the signer. It continuously receives events from
    /// `event_recv`, polling for up to `self.get_event_timeout()` units of time.  Once it has
    /// polled for events, they are fed into `run_one_pass()`.  This continues until either
    /// `run_one_pass()` returns `false`, or the event receiver hangs up.  At this point, this
    /// method calls the `event_stop_signaler.send()` to terminate the receiver.
    ///
    /// This would run in a separate thread from the event receiver.
    fn main_loop<EVST: EventStopSignaler>(
        &mut self,
        event_recv: Receiver<StackerDBChunksEvent>,
        command_recv: Receiver<CMD>,
        mut event_stop_signaler: EVST,
    ) -> Option<R> {
        loop {
            let poll_timeout = self.get_event_timeout();
            info!("SignerRunLoop::main_loop: poll_timeout {:?}", poll_timeout);
            info!("SignerRunLoop::main_loop: event_recv");
            let next_event_opt = match event_recv.recv_timeout(poll_timeout) {
                Ok(event) => Some(event),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    info!("Event receiver disconnected");
                    return None;
                }
            };
            info!("SignerRunLoop::main_loop: command_recv");
            let next_command_opt = match command_recv.recv_timeout(poll_timeout) {
                Ok(cmd) => Some(cmd),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    info!("Command receiver disconnected");
                    return None;
                }
            };
            info!("SignerRunLoop::main_loop: run_one_pass");
            if let Some(final_state) = self.run_one_pass(next_event_opt, next_command_opt) {
                // finished!
                info!("Runloop exit; signaling event-receiver to stop");
                event_stop_signaler.send();
                return Some(final_state);
            }
        }
    }
}

/// The top-level signer implementation
pub struct Signer<CMD: Send, R, SL: SignerRunLoop<R, CMD> + Send + Sync, EV: EventReceiver + Send> {
    /// the runloop itself
    signer_loop: Option<SL>,
    /// the event receiver to use
    event_receiver: Option<EV>,
    /// the command receiver to use
    command_receiver: Option<Receiver<CMD>>,
    /// marker to permit the R type
    _phantom: PhantomData<R>,
}

/// The running signer implementation
pub struct RunningSigner<EV: EventReceiver, R> {
    /// join handle for signer runloop
    signer_join: JoinHandle<Option<R>>,
    /// join handle for event receiver
    event_join: JoinHandle<()>,
    /// kill signal for the event receiver
    stop_signal: EV::ST,
}

impl<EV: EventReceiver, R> RunningSigner<EV, R> {
    /// Stop the signer, and get the final state
    pub fn stop(mut self) -> Option<R> {
        // kill event receiver
        self.stop_signal.send();

        debug!("Try join event loop...");
        // wait for event receiver join
        let _ = self.event_join.join().map_err(|thread_panic| {
            error!("Event thread panicked with: '{:?}'", &thread_panic);
            thread_panic
        });
        info!("Event receiver thread joined");

        // wait for runloop to join
        debug!("Try join signer loop...");
        let result_opt = self
            .signer_join
            .join()
            .map_err(|thread_panic| {
                error!("Event thread panicked with: '{:?}'", &thread_panic);
                thread_panic
            })
            .unwrap_or(None);

        info!("Signer thread joined");
        result_opt
    }
}

/// Write to stderr in an async-safe manner.
/// See signal-safety(7)
#[warn(unused)]
fn async_safe_write_stderr(msg: &str) {
    #[cfg(windows)]
    unsafe {
        // write(2) inexplicably has a different ABI only on Windows.
        libc::write(
            STDERR,
            msg.as_ptr() as *const libc::c_void,
            msg.len() as u32,
        );
    }
    #[cfg(not(windows))]
    unsafe {
        libc::write(STDERR, msg.as_ptr() as *const libc::c_void, msg.len());
    }
}

/// This is a termination handler for handling receipt of a terminating UNIX signal, like SIGINT,
/// SIGQUIT, SIGTERM, or SIGBUS.  You'd call this as part of the startup code for the signer daemon.
/// DO NOT call this from within the library!
pub fn set_runloop_signal_handler<ST: EventStopSignaler + Send + 'static>(mut stop_signaler: ST) {
    termination::set_handler(move |sig_id| match sig_id {
        SignalId::Bus => {
            let msg = "Caught SIGBUS; crashing immediately and dumping core\n";
            async_safe_write_stderr(msg);
            unsafe {
                libc::abort();
            }
        }
        _ => {
            let msg = format!("Graceful termination request received (signal `{}`), will complete the ongoing runloop cycles and terminate\n", sig_id);
            async_safe_write_stderr(&msg);
            stop_signaler.send();
        }
    }).expect("FATAL: failed to set signal handler");
}

impl<
        CMD: Send + 'static,
        R: Send + 'static,
        SL: SignerRunLoop<R, CMD> + Send + Sync + 'static,
        EV: EventReceiver + Send + 'static,
    > Signer<CMD, R, SL, EV>
{
    pub fn new(runloop: SL, event_receiver: EV, command_receiver: Receiver<CMD>) -> Signer<CMD, R, SL, EV> {
        Signer {
            signer_loop: Some(runloop),
            event_receiver: Some(event_receiver),
            command_receiver: Some(command_receiver),
            _phantom: PhantomData,
        }
    }

    /// This is a helper function to spawn both the runloop and event receiver in their own
    /// threads.  Advanced signers may not need this method, and instead opt to run the receiver
    /// and runloop directly.  However, this method is present to help signer developers to get
    /// their implementations off the ground.
    ///
    /// The given `bind_addr` is the server address this event receiver needs to listen on, so the
    /// stacks node can POST events to it.
    ///
    /// On success, this method consumes the Signer and returns a RunningSigner with the relevant
    /// inter-thread communication primitives for the caller to shut down the system.
    pub fn spawn(&mut self, bind_addr: SocketAddr) -> Result<RunningSigner<EV, R>, EventError> {
        let mut event_receiver = self
            .event_receiver
            .take()
            .ok_or(EventError::AlreadyRunning)?;
        let command_receiver = self
            .command_receiver
            .take()
            .ok_or(EventError::AlreadyRunning)?;
        let mut signer_loop = self.signer_loop.take().ok_or(EventError::AlreadyRunning)?;

        let (event_send, event_recv) = channel();
        event_receiver.add_consumer(event_send);

        event_receiver.bind(bind_addr)?;
        let stop_signaler = event_receiver.get_stop_signaler()?;
        let mut ret_stop_signaler = event_receiver.get_stop_signaler()?;

        // start a thread for the event receiver
        let event_thread = thread::Builder::new()
            .name("event_receiver".to_string())
            .stack_size(THREAD_STACK_SIZE)
            .spawn(move || event_receiver.main_loop())
            .map_err(|e| {
                error!("EventReceiver failed to start: {:?}", &e);
                EventError::FailedToStart
            })?;

        // start receiving events and doing stuff with them
        let runloop_thread = thread::Builder::new()
            .name("signer_runloop".to_string())
            .stack_size(THREAD_STACK_SIZE)
            .spawn(move || signer_loop.main_loop(event_recv, command_receiver, stop_signaler))
            .map_err(|e| {
                error!("SignerRunLoop failed to start: {:?}", &e);
                ret_stop_signaler.send();
                EventError::FailedToStart
            })?;

        let running_signer = RunningSigner {
            signer_join: runloop_thread,
            event_join: event_thread,
            stop_signal: ret_stop_signaler,
        };

        Ok(running_signer)
    }
}
