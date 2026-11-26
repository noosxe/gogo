//! Drives a [`Model`] over the fake backend the way a shell does.

#![allow(dead_code)] // Each test file uses its own part.

use gogo_contract::{PlanFacts, Value, actions};
use gogo_gui_model::fake::{FakeBackend, FakePlan};
use gogo_gui_model::{InputEvent, Invocation, Modal, Model, Outcome, Strength, Ticket};

pub struct App {
    pub model: Model<FakeBackend>,
    last: u64,
}

impl App {
    pub fn new() -> Self {
        let mut model = Model::new(FakeBackend::new(), || {});
        // So that gated actions can be tried; the gate has its own tests.
        model.backend_mut().gates.cloud_writes = true;
        App { model, last: 0 }
    }

    pub fn fake(&mut self) -> &mut FakeBackend {
        self.model.backend_mut()
    }

    /// The next real input.
    pub fn input(&mut self) -> InputEvent {
        self.last += 1;
        InputEvent(self.last)
    }

    pub fn last_input(&self) -> InputEvent {
        InputEvent(self.last)
    }

    /// A user activating a control bound to `invocation`, then a frame.
    pub fn invoke(&mut self, invocation: &Invocation) -> Outcome {
        let event = self.input();
        let outcome = self.model.invoke(invocation, event);
        self.model.pump();
        outcome
    }

    pub fn ticket(&self) -> Ticket {
        self.model.modal().expect("a screen").ticket()
    }

    /// The shell has drawn the screen on top.
    pub fn draw(&mut self) -> Ticket {
        let ticket = self.ticket();
        self.model.presented(ticket, self.last_input());
        ticket
    }

    /// Draw the plan card and press its confirm control.
    pub fn confirm(&mut self) -> Outcome {
        let ticket = self.draw();
        let event = self.input();
        let outcome = self.model.confirm(ticket, event);
        self.model.pump();
        outcome
    }

    pub fn screen(&self) -> &'static str {
        match self.model.modal() {
            None => "none",
            Some(Modal::Prompt { .. }) => "prompt",
            Some(Modal::Planning { .. }) => "planning",
            Some(Modal::PlanCard { .. }) => "card",
            Some(Modal::Strong { .. }) => "strong",
            Some(Modal::Error { .. }) => "error",
        }
    }

    pub fn strength(&self) -> Strength {
        match self.model.modal() {
            Some(Modal::Strong { strength, .. }) => strength.clone(),
            other => panic!("no strong confirmation: {other:?}"),
        }
    }

    pub fn runs(&self) -> usize {
        self.model.backend().runs.len()
    }

    /// The names of what ran.
    pub fn ran(&self) -> Vec<&'static str> {
        self.model
            .backend()
            .runs
            .iter()
            .map(|r| r.call.name())
            .collect()
    }
}

/// An invocation of `name` with a good value for every argument it needs.
pub fn call(name: &str) -> Invocation {
    use actions::ParamType as T;
    let spec = actions::action(name).expect("an action");
    let mut invocation = Invocation::new(spec);
    for p in spec.params {
        if p.optional || p.ty == T::RouteParams {
            continue;
        }
        let value = match p.ty {
            T::Route => Value::str("queue"),
            T::Bool => Value::Bool(true),
            T::Index | T::Indices | T::Bytes => Value::Num(1.0),
            T::OneOf(words) => Value::str(words[0]),
            T::Selection => Value::Map(Default::default()),
            T::SettingKey => Value::str("gui.mode"),
            _ => Value::str("1"),
        };
        invocation = invocation.arg(p.name, value);
    }
    invocation
}

/// A plan that removes DLC: destructive for the actions that escalate on it.
pub fn removes_dlc() -> FakePlan {
    FakePlan::with(PlanFacts {
        removes_dlc: true,
        ..PlanFacts::changes()
    })
}
