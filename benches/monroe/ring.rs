use std::future::Future;

use monroe::{
    runtime::{self as rt, tokio::Runtime, Runtime as _},
    supervisor::NoRestart,
    Actor, Address, Context,
};

use crate::RingSpec;

pub fn run(spec: RingSpec) {
    Runtime::new()
        .expect("Failed to create actor runtime")
        .block_on(|handle| setup_actors(handle, spec))
}

async fn setup_actors(handle: rt::Handle<Runtime>, spec: RingSpec) {
    // Spawn all the requested actors for the ring.
    let first = handle
        .spawn_actor(NoRestart, Ring::new as fn(&mut _, _, _) -> _, (1, spec))
        .unwrap();

    // Close the ring by forwarding the first actor's
    // address to the last actor in the chain.
    first.tell(first.clone()).await.unwrap();

    // Throw the first payload into the ring.
    first.tell(0).await.unwrap();

    // Wait for the actors to finish doing their thing.
    // TODO: How can we do this less clunky?
    while first.is_connected() {
        tokio::task::yield_now().await;
    }
}

#[derive(Clone, Debug)]
enum Message {
    Payload(i32),
    CloseRing(Address<Ring>),
}

// FIXME: Issue #5

impl From<i32> for Message {
    fn from(value: i32) -> Self {
        Message::Payload(value)
    }
}

impl From<Address<Ring>> for Message {
    fn from(value: Address<Ring>) -> Self {
        Message::CloseRing(value)
    }
}

struct Ring {
    next: Option<Address<Self>>,
    id: i32,
    spec: RingSpec,
}

impl Ring {
    pub fn new(_: &mut Context<Self>, id: i32, spec: RingSpec) -> Self {
        Self {
            next: None,
            id,
            spec,
        }
    }
}

impl Actor for Ring {
    type Message = Message;
    type Error = !;
    type Runtime = Runtime;
    type Fut<'a> = impl Future<Output = Result<(), !>> + 'a;

    fn run<'a>(&'a mut self, ctx: &'a mut Context<Self>) -> Self::Fut<'a> {
        async move {
            if self.id < self.spec.nodes {
                self.next = Some(ctx.runtime().spawn_actor(
                    NoRestart,
                    Ring::new as fn(&mut _, _, _) -> _,
                    (self.id + 1, self.spec),
                )?);
            }

            while let Ok(msg) = ctx.recv_next().await {
                match msg {
                    Message::CloseRing(addr) => match self.next {
                        Some(ref next) => next.tell(addr).await.unwrap(),
                        None => self.next = Some(addr),
                    },

                    Message::Payload(value) => {
                        if value >= self.spec.limit {
                            ctx.runtime().stop();
                            break;
                        }

                        self.next.as_ref().unwrap().tell(value + 1).await.unwrap();
                    }
                }
            }

            Ok(())
        }
    }
}
