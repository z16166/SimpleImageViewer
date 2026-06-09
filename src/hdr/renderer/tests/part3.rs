use super::*;

fn test_hdr_renderer_multi_binding_and_lru_eviction() {
    let Some((_instance, _adapter, device, queue)) = pollster::block_on(async {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: true,
                compatible_surface: None,
            })
            .await
            .ok()?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .ok()?;
        Some((instance, adapter, device, queue))
    }) else {
        log::warn!("Skipping GPU test: no adapter available");
        return;
    };

    let mut callback_resources = CallbackResources::default();
    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    callback_resources.insert(create_callback_resources(&device, target_format));

    let images: Vec<_> = (1..=9)
        .map(|i| {
            let size = i * 10;
            let pixels = (size * size * 4) as usize;
            Arc::new(hdr_image(
                size,
                size,
                HdrPixelFormat::Rgba32Float,
                vec![1.0; pixels],
            ))
        })
        .collect();

    let screen_desc = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [100, 100],
        pixels_per_point: 1.0,
    };

    // Prepare eight callbacks (sleeping so LRU timestamps are distinct).
    for (i, img) in images.iter().take(8).enumerate() {
        if i > 0 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let callback = HdrImagePlaneCallback {
            image: Arc::clone(img),
            tone_map: HdrToneMapSettings::default(),
            target_format,
            output_mode: HdrRenderOutputMode::SdrToneMapped,
            rotation_steps: 0,
            alpha: 1.0,
            uv_rect: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            ripple: None,
            keep_resident: false,
        };

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let cmds = callback.prepare(
            &device,
            &queue,
            &screen_desc,
            &mut encoder,
            &mut callback_resources,
        );
        if !cmds.is_empty() {
            queue.submit(cmds);
        }
    }

    // Verify that we have exactly eight bindings in resources and they are independent
    {
        let resources = callback_resources.get::<HdrCallbackResources>().unwrap();
        assert_eq!(resources.image_bindings.len(), 8);

        let key0 = HdrImageKey::from_image(&images[0]);
        let key1 = HdrImageKey::from_image(&images[1]);
        let key7 = HdrImageKey::from_image(&images[7]);

        let b0 = resources.image_bindings.get(&key0).unwrap();
        let b1 = resources.image_bindings.get(&key1).unwrap();
        let b7 = resources.image_bindings.get(&key7).unwrap();

        assert!(b0.bind_group.is_some());
        assert!(b1.bind_group.is_some());
        assert!(b7.bind_group.is_some());

        assert_eq!(b0.uploaded_texture.width(), 10);
        assert_eq!(b1.uploaded_texture.width(), 20);
        assert_eq!(b7.uploaded_texture.width(), 80);
    }

    // Age out the oldest binding past the eviction-protect window, then insert a ninth image.
    std::thread::sleep(std::time::Duration::from_millis(60));

    // Now prepare the 9th image callback. This should trigger eviction of the oldest (the 1st one)
    {
        let callback = HdrImagePlaneCallback {
            image: Arc::clone(&images[8]),
            tone_map: HdrToneMapSettings::default(),
            target_format,
            output_mode: HdrRenderOutputMode::SdrToneMapped,
            rotation_steps: 0,
            alpha: 1.0,
            uv_rect: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            ripple: None,
            keep_resident: false,
        };

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let cmds = callback.prepare(
            &device,
            &queue,
            &screen_desc,
            &mut encoder,
            &mut callback_resources,
        );
        if !cmds.is_empty() {
            queue.submit(cmds);
        }
    }

    // Verify that resources has size 8 and images[0] has been evicted
    {
        let resources = callback_resources.get::<HdrCallbackResources>().unwrap();
        assert_eq!(resources.image_bindings.len(), 8);

        let key_evicted = HdrImageKey::from_image(&images[0]);
        assert!(!resources.image_bindings.contains_key(&key_evicted));

        for img in images.iter().skip(1) {
            let key = HdrImageKey::from_image(img);
            assert!(resources.image_bindings.contains_key(&key));
        }
    }
}
