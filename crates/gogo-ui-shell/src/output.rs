//! Which renderer draws, and what to do when one fails (design 24 §4).

use crate::{RenderError, RendererChoice};

/// The renderer in use.
pub(crate) enum Output<G, C> {
    Gpu(G),
    Cpu(C),
}

/// Picks the renderer for `choice`, making each from `cx`. The CPU one is only made when
/// it is needed.
pub(crate) fn open<X, G, C>(
    choice: RendererChoice,
    cx: &mut X,
    gpu: impl FnOnce(&mut X) -> Result<G, RenderError>,
    cpu: impl FnOnce(&mut X) -> Result<C, RenderError>,
) -> Result<Output<G, C>, RenderError> {
    match choice {
        RendererChoice::Gpu => gpu(cx).map(Output::Gpu),
        RendererChoice::Cpu => cpu(cx).map(Output::Cpu),
        RendererChoice::Auto => match gpu(cx) {
            Ok(g) => Ok(Output::Gpu(g)),
            Err(e) => {
                tracing::warn!("no GPU renderer, using the CPU: {e}");
                cpu(cx).map(Output::Cpu)
            }
        },
    }
}

/// What to do after the GPU renderer failed on a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Recovery {
    /// Make the GPU renderer again.
    Remake,
    /// Switch to the CPU for the rest of the run.
    UseCpu,
    /// Stop: the GPU was asked for and does not work.
    Stop,
}

/// `remade` says whether the renderer was already made again since the last good frame.
pub(crate) fn recover(choice: RendererChoice, remade: bool) -> Recovery {
    match (remade, choice) {
        (false, _) => Recovery::Remake,
        (true, RendererChoice::Gpu) => Recovery::Stop,
        (true, _) => Recovery::UseCpu,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(choice: RendererChoice, gpu_works: bool) -> (Option<&'static str>, bool) {
        let mut made_cpu = false;
        let out = open(
            choice,
            &mut made_cpu,
            |_| {
                if gpu_works {
                    Ok(())
                } else {
                    Err(RenderError::from("no adapter"))
                }
            },
            |made| {
                *made = true;
                Ok(())
            },
        );
        let name = out.ok().map(|o| match o {
            Output::Gpu(()) => "gpu",
            Output::Cpu(()) => "cpu",
        });
        (name, made_cpu)
    }

    #[test]
    fn auto_prefers_the_gpu_and_falls_back() {
        assert_eq!(pick(RendererChoice::Auto, true), (Some("gpu"), false));
        assert_eq!(pick(RendererChoice::Auto, false), (Some("cpu"), true));
    }

    #[test]
    fn a_forced_renderer_is_the_only_one_made() {
        assert_eq!(pick(RendererChoice::Gpu, false), (None, false));
        assert_eq!(pick(RendererChoice::Cpu, true), (Some("cpu"), true));
    }

    #[test]
    fn a_failing_gpu_is_remade_once() {
        for choice in [RendererChoice::Auto, RendererChoice::Gpu] {
            assert_eq!(recover(choice, false), Recovery::Remake);
        }
        assert_eq!(recover(RendererChoice::Auto, true), Recovery::UseCpu);
        assert_eq!(recover(RendererChoice::Gpu, true), Recovery::Stop);
    }
}
