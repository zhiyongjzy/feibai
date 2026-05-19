use std::fs::File;
use std::os::unix::io::AsFd;

use wayland_client::protocol::{wl_buffer, wl_compositor, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::QueueHandle;
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_v2::ZwpInputMethodV2,
    zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2,
};

use feibai_core::Candidate;
use feibai_ui::{CandidateRenderer, RenderedFrame, Theme};

use crate::State;

pub struct PopupWindow {
    surface: Option<wl_surface::WlSurface>,
    popup: Option<ZwpInputPopupSurfaceV2>,
    buffer: Option<wl_buffer::WlBuffer>,
    pool: Option<wl_shm_pool::WlShmPool>,
    pool_fd: Option<File>,
    pool_size: usize,
    buf_width: u32,
    buf_height: u32,
    renderer: CandidateRenderer,
    visible: bool,
}

impl PopupWindow {
    pub fn new_with_theme(theme: Theme) -> Self {
        Self {
            surface: None,
            popup: None,
            buffer: None,
            pool: None,
            pool_fd: None,
            pool_size: 0,
            buf_width: 0,
            buf_height: 0,
            renderer: CandidateRenderer::new(theme.config()),
            visible: false,
        }
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.renderer.set_config(theme.config());
    }

    pub fn create_surface(
        &mut self,
        compositor: &wl_compositor::WlCompositor,
        input_method: &ZwpInputMethodV2,
        qh: &QueueHandle<State>,
    ) {
        if self.surface.is_some() {
            return;
        }
        let surface = compositor.create_surface(qh, ());
        let popup = input_method.get_input_popup_surface(&surface, qh, ());
        self.surface = Some(surface);
        self.popup = Some(popup);
    }

    pub fn destroy(&mut self) {
        if let Some(buf) = self.buffer.take() {
            buf.destroy();
        }
        if let Some(pool) = self.pool.take() {
            pool.destroy();
        }
        if let Some(popup) = self.popup.take() {
            popup.destroy();
        }
        if let Some(surface) = self.surface.take() {
            surface.destroy();
        }
        self.pool_fd = None;
        self.pool_size = 0;
        self.visible = false;
    }

    pub fn show(
        &mut self,
        preedit: &str,
        candidates: &[Candidate],
        selected: usize,
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<State>,
    ) {
        if self.surface.is_none() {
            return;
        }

        let frame = match self.renderer.render(preedit, candidates, selected) {
            Some(f) => f,
            None => {
                self.hide();
                return;
            }
        };

        let width = frame.width;
        let height = frame.height;
        self.update_buffer(&frame, shm, qh);

        let surface = self.surface.as_ref().unwrap();
        if let Some(buf) = &self.buffer {
            surface.attach(Some(buf), 0, 0);
            surface.damage_buffer(0, 0, width as i32, height as i32);
            surface.commit();
            self.visible = true;
        }
    }

    pub fn hide(&mut self) {
        if !self.visible {
            return;
        }
        if let Some(surface) = &self.surface {
            surface.attach(None, 0, 0);
            surface.commit();
        }
        self.visible = false;
    }

    fn update_buffer(
        &mut self,
        frame: &RenderedFrame,
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<State>,
    ) {
        let stride = frame.width * 4;
        let size = (stride * frame.height) as usize;

        // Recreate pool if needed
        if self.pool_size < size {
            if let Some(buf) = self.buffer.take() {
                buf.destroy();
            }
            if let Some(pool) = self.pool.take() {
                pool.destroy();
            }

            let fd = create_shm_file(size);
            let pool = shm.create_pool(fd.as_fd(), size as i32, qh, ());
            self.pool_fd = Some(fd);
            self.pool = Some(pool);
            self.pool_size = size;
            self.buf_width = 0;
            self.buf_height = 0;
        }

        // Recreate buffer if dimensions changed
        if self.buf_width != frame.width || self.buf_height != frame.height {
            if let Some(buf) = self.buffer.take() {
                buf.destroy();
            }
        }

        if self.buffer.is_none()
            && let Some(pool) = &self.pool
        {
            let buf = pool.create_buffer(
                0,
                frame.width as i32,
                frame.height as i32,
                stride as i32,
                wl_shm::Format::Argb8888,
                qh,
                (),
            );
            self.buffer = Some(buf);
            self.buf_width = frame.width;
            self.buf_height = frame.height;
        }

        // Write pixel data to mmap
        if let Some(fd) = &self.pool_fd {
            use std::os::unix::io::AsRawFd;
            let raw_fd = fd.as_raw_fd();
            unsafe {
                let ptr = libc::mmap(
                    std::ptr::null_mut(),
                    size,
                    libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    raw_fd,
                    0,
                );
                if ptr != libc::MAP_FAILED {
                    std::ptr::copy_nonoverlapping(frame.data.as_ptr(), ptr as *mut u8, size);
                    libc::munmap(ptr, size);
                }
            }
        }
    }
}

fn create_shm_file(size: usize) -> File {
    let name = format!("/feibai-shm-{}", std::process::id());
    let fd = rustix::shm::open(
        &name,
        rustix::shm::OFlags::CREATE | rustix::shm::OFlags::RDWR | rustix::shm::OFlags::EXCL,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .expect("shm_open failed");
    rustix::shm::unlink(&name).ok();

    let file = File::from(fd);
    rustix::fs::ftruncate(&file, size as u64).expect("ftruncate failed");
    file
}
