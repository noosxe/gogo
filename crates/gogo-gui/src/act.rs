//! What a skin control asks for, as the model takes it (design 24 §2: `gogo-gui`
//! translates between the engine's ids and the app's invocations; design 25 §4).

use gogo_gui_model::{InputEvent, Invocation};
use gogo_skin::expr::Scope;
use gogo_skin::{ActionCall, ArgValue, Input, PendingError};
use gogo_ui_interact::InputEvent as EngineEvent;

use crate::data::FIXTURE_EVENT;

/// The model's number for a real input. The model counts one input already: the one that
/// put the fixture on its game.
pub(crate) fn number(event: EngineEvent) -> InputEvent {
    InputEvent(event.get() + FIXTURE_EVENT)
}

/// The call of a skin control or a popup's entry, with the arguments that wait for the user's input worked
/// out from it.
pub(crate) fn invocation(
    call: &ActionCall,
    data: &dyn Scope,
    input: &Input,
) -> Result<Invocation, PendingError> {
    let mut invocation = Invocation::new(call.spec);
    for arg in &call.args {
        let value = match &arg.value {
            ArgValue::Value(value) => value.clone(),
            ArgValue::OnInput(pending) => pending.eval(data, input)?,
        };
        if value == gogo_contract::Value::Null && arg.param.optional {
            continue;
        }
        invocation = match arg.route_param {
            Some(param) => invocation.route_arg(param.name, value),
            None => invocation.arg(arg.param.name, value),
        };
    }
    Ok(invocation)
}
