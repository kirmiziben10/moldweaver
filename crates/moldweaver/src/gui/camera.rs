//! Orbit camera (Z up).

use glam::{Mat4, Vec2, Vec3};

#[derive(Clone, Copy, Debug)]
pub struct OrbitCamera {
    pub target: Vec3,
    /// Rotation around Z, radians.
    pub yaw: f32,
    /// Elevation above the XY plane, radians.
    pub pitch: f32,
    pub distance: f32,
    pub fov_y: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        OrbitCamera { target: Vec3::ZERO, yaw: -0.9, pitch: 0.45, distance: 200.0, fov_y: 35f32.to_radians() }
    }
}

impl OrbitCamera {
    pub fn eye(&self) -> Vec3 {
        let dir = Vec3::new(self.pitch.cos() * self.yaw.cos(), self.pitch.cos() * self.yaw.sin(), self.pitch.sin());
        self.target + dir * self.distance
    }

    pub fn view(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye(), self.target, Vec3::Z)
    }

    pub fn proj(&self, aspect: f32) -> Mat4 {
        let near = (self.distance * 0.01).max(0.05);
        let far = self.distance * 20.0 + 1000.0;
        Mat4::perspective_rh(self.fov_y, aspect.max(1e-3), near, far)
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.proj(aspect) * self.view()
    }

    pub fn orbit(&mut self, delta: Vec2) {
        self.yaw -= delta.x * 0.008;
        self.pitch = (self.pitch + delta.y * 0.008).clamp(-1.55, 1.55);
    }

    /// Pan by a screen-space delta (pixels) for a viewport of `height` pixels.
    pub fn pan(&mut self, delta: Vec2, height: f32) {
        let scale = 2.0 * self.distance * (self.fov_y * 0.5).tan() / height.max(1.0);
        let view = self.view();
        let right = Vec3::new(view.x_axis.x, view.y_axis.x, view.z_axis.x);
        let up = Vec3::new(view.x_axis.y, view.y_axis.y, view.z_axis.y);
        self.target += (-right * delta.x + up * delta.y) * scale;
    }

    pub fn zoom(&mut self, steps: f32) {
        self.distance = (self.distance * (-steps * 0.0015).exp()).clamp(1.0, 50_000.0);
    }

    /// Frame a bounding sphere.
    pub fn fit(&mut self, center: Vec3, radius: f32) {
        self.target = center;
        self.distance = (radius.max(1.0) / (self.fov_y * 0.5).sin()) * 1.1;
    }

    /// World-space ray through a point in normalized device coordinates.
    pub fn ray(&self, ndc: Vec2, aspect: f32) -> (Vec3, Vec3) {
        let inv = self.view_proj(aspect).inverse();
        let a = inv.project_point3(ndc.extend(0.0));
        let b = inv.project_point3(ndc.extend(1.0));
        (a, (b - a).normalize())
    }
}
