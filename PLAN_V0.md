Person A — Core + Integration (emu.rs, main.rs, pacer.rs)
This is the critical path, but only because it defines the contract, not because it's the most work. Their first job on day 1 is publishing the Frame, KeyState, and Renderer trait signatures — even before Emu::load actually works — so B and C aren't blocked. They own the final integration loop, which is unavoidably a serialization point near the end: someone has to wire everyone's pieces together and that can't really be parallelized further.
Person B — Video (render.rs: both HalfBlockRenderer and KittyRenderer)
Fully mockable from hour one — feed it a synthetic framebuffer (checkerboard pattern, gradient, whatever) and iterate on both renderers without a working emulator core at all. This is genuinely the biggest chunk of work (Kitty protocol quirks are the thing most likely to eat unplanned time from earlier estimates), so it's reasonable to make this its own full workstream rather than splitting it further.
Person C — Input + Audio (input.rs, audio.rs)
Also independently testable: input.rs just needs a terminal and can be verified by printing pressed keys to a log, no emulator needed. audio.rs can be verified by pushing a synthetic sine wave through cpal before any real PCM samples exist. These two are smaller individually, which is why they're bundled — otherwise C is underloaded relative to A and B.

