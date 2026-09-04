//! GPU scheduling priority. A game that keeps the GPU near 100 percent
//! starves the capture copy and the encoder, which shows up as repeated
//! frames. Windows lets a process ask for a higher GPU scheduling class;
//! the higher classes need administrator rights, so this is best effort.

use tracing::info;

#[cfg(windows)]
pub fn raise_gpu_priority() {
    use windows::Wdk::Graphics::Direct3D::{
        D3DKMT_SCHEDULINGPRIORITYCLASS_ABOVE_NORMAL, D3DKMT_SCHEDULINGPRIORITYCLASS_HIGH,
        D3DKMTSetProcessSchedulingPriorityClass,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: plain kernel calls on the current process handle.
    unsafe {
        let process = GetCurrentProcess();
        for (class, name) in [
            (D3DKMT_SCHEDULINGPRIORITYCLASS_HIGH, "high"),
            (D3DKMT_SCHEDULINGPRIORITYCLASS_ABOVE_NORMAL, "above normal"),
        ] {
            if D3DKMTSetProcessSchedulingPriorityClass(process, class).is_ok() {
                info!("GPU scheduling priority set to {name}");
                return;
            }
        }
    }
    info!("GPU scheduling priority left at normal; run as administrator to raise it");
}

#[cfg(not(windows))]
pub fn raise_gpu_priority() {}
