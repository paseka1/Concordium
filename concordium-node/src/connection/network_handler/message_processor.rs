use crate::{
    connection::{
        network_handler::{
            message_handler::{
                NetworkMessageCW, NetworkPacketCW, NetworkRequestCW, NetworkResponseCW,
            },
            MessageHandler,
        },
        Connection,
    },
    network::message::NetworkMessage,
};
use concordium_common::{
    filters::{Filter, FilterAFunc, FilterResult, Filters},
    functor::{FuncResult, UnitFunction, UnitFunctor},
    UCursor,
};
use failure::Fallible;
use std::{
    rc::Rc,
    sync::{Arc, RwLock},
};

/// Models the result of processing a message through the `MessageProcessor`
#[derive(Debug)]
pub enum ProcessResult {
    Drop,
    Done,
}

pub fn collapse_process_result(
    conn: &mut Connection,
    data: Vec<UCursor>,
) -> Result<ProcessResult, Vec<Result<ProcessResult, failure::Error>>> {
    let mut found_drop = false;
    let mut errors = vec![];

    for elem in data {
        let res = conn.process_message(elem);
        if res.is_err() {
            errors.push(res);
        } else if let Ok(ProcessResult::Drop) = res {
            found_drop = true;
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    if found_drop {
        Ok(ProcessResult::Drop)
    } else {
        Ok(ProcessResult::Done)
    }
}

/// Handles a `NetworkMessage`
///
/// First, the message is passed to filters sorted in descending priority. Only
/// of all the filters are passed, then the execution part begins. All the
/// actions are executed (the inner `MessageHandler` dispatchs to several
/// functors depending on the variant of the `NetworkMessage`) and in the end
/// the `notifications` receive the message in case they want to react to the
/// processing of the message finishing.
pub struct MessageProcessor {
    filters:       Filters<NetworkMessage>,
    actions:       MessageHandler,
    notifications: UnitFunctor<NetworkMessage>,
}

impl Default for MessageProcessor {
    fn default() -> Self { MessageProcessor::new() }
}

impl MessageProcessor {
    pub fn new() -> Self {
        MessageProcessor {
            filters:       Filters::new(),
            actions:       MessageHandler::new(),
            notifications: UnitFunctor::new(),
        }
    }

    pub fn add_filter(&mut self, func: FilterAFunc<NetworkMessage>, priority: u8) -> &mut Self {
        self.filters.add_filter(func, priority);
        self
    }

    pub fn push_filter(&mut self, filter: Filter<NetworkMessage>) -> &mut Self {
        self.filters.push_filter(filter);
        self
    }

    pub fn filters(&self) -> &[Filter<NetworkMessage>] { self.filters.get_filters() }

    pub fn add_request_action(&mut self, callback: NetworkRequestCW) -> &mut Self {
        self.actions.add_request_callback(callback);
        self
    }

    pub fn add_response_action(&mut self, callback: NetworkResponseCW) -> &mut Self {
        self.actions.add_response_callback(callback);
        self
    }

    pub fn add_packet_action(&mut self, callback: NetworkPacketCW) -> &mut Self {
        self.actions.add_packet_callback(callback);
        self
    }

    pub fn add_action(&mut self, callback: NetworkMessageCW) -> &mut Self {
        self.actions.add_callback(callback);
        self
    }

    pub fn set_invalid_handler(&mut self, func: Rc<(Fn() -> FuncResult<()>)>) -> &mut Self {
        self.actions.set_invalid_handler(func);
        self
    }

    pub fn set_unknown_handler(&mut self, func: Rc<(Fn() -> FuncResult<()>)>) -> &mut Self {
        self.actions.set_unknown_handler(func);
        self
    }

    pub fn actions(&self) -> &MessageHandler { &self.actions }

    pub fn add_notification(&mut self, func: UnitFunction<NetworkMessage>) -> &mut Self {
        self.notifications.add_callback(func);
        self
    }

    pub fn notifications(&self) -> &[UnitFunction<NetworkMessage>] {
        self.notifications.callbacks()
    }

    pub fn process_message(&mut self, message: &NetworkMessage) -> Fallible<ProcessResult> {
        if FilterResult::Pass == self.filters.run_filters(message)? {
            self.actions.process_message(message)?;
            self.notifications.run_callbacks(message)?;
            Ok(ProcessResult::Done)
        } else {
            Ok(ProcessResult::Drop)
        }
    }

    pub fn add(&mut self, other: &MessageProcessor) -> &mut Self {
        for cb in other.filters().iter() {
            self.push_filter(cb.clone());
        }

        self.actions.add(&other.actions);

        for cb in other.notifications().iter() {
            self.add_notification(Arc::clone(&cb));
        }

        self
    }
}

pub trait MessageManager {
    fn message_processor(&self) -> Arc<RwLock<MessageProcessor>>;
}