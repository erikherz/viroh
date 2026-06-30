//! End-to-end checks for the codec, framing, and (live) iroh transport.

use viroh::{read_frame, read_meta, video, write_frame, write_meta, StreamMeta, ALPN};

#[test]
fn codec_roundtrip_preserves_dimensions_and_content() {
    let mut src = video::TimecodeSource::new(640, 480, 30);
    let frame = src.render(3_661_123); // 01:01:01.123

    let jpeg = video::encode_jpeg(&frame, 80).expect("encode");
    assert!(jpeg.starts_with(&[0xFF, 0xD8]), "not a JPEG (missing SOI)");

    let decoded = video::decode_jpeg(&jpeg).expect("decode");
    assert_eq!((decoded.width, decoded.height), (640, 480));

    // The center of the frame holds the bright green timecode glyphs, so the
    // decoded image must not be uniformly black there.
    let mut bright = 0;
    for y in 200..280 {
        for x in 0..640 {
            let i = (y * 640 + x) * 3;
            if decoded.rgb[i + 1] > 100 {
                bright += 1;
            }
        }
    }
    assert!(bright > 200, "expected visible timecode pixels, got {bright}");
}

#[test]
fn timecode_formatting() {
    assert_eq!(video::format_timecode(0), "00:00:00.000");
    assert_eq!(video::format_timecode(3_661_123), "01:01:01.123");
    assert_eq!(video::format_timecode(86_399_999), "23:59:59.999");
}

#[tokio::test]
async fn framing_roundtrip() {
    let (mut a, mut b) = tokio::io::duplex(1 << 16);
    let payload = vec![1u8, 2, 3, 4, 5, 6, 7];
    let p2 = payload.clone();
    tokio::spawn(async move {
        write_frame(&mut a, &p2).await.unwrap();
        write_frame(&mut a, &[]).await.unwrap(); // empty frame is valid
        drop(a);
    });
    assert_eq!(read_frame(&mut b).await.unwrap().unwrap(), payload);
    assert_eq!(read_frame(&mut b).await.unwrap().unwrap(), Vec::<u8>::new());
    assert!(read_frame(&mut b).await.unwrap().is_none(), "clean EOF");
}

/// Live transport check: two in-process endpoints with direct (LAN) addresses,
/// no relay/discovery. Ignored by default because it needs to bind UDP sockets;
/// run with `cargo test -- --ignored`.
#[tokio::test]
#[ignore]
async fn iroh_loopback_streams_frames() {
    use iroh::{endpoint::presets, Endpoint, EndpointAddr, TransportAddr};

    // Bind both endpoints on loopback and dial the exact bound socket, so the
    // test is fully self-contained (no relay, no LAN, no discovery).
    let server = Endpoint::builder(presets::Minimal)
        .alpns(vec![ALPN.to_vec()])
        .bind_addr("127.0.0.1:0")
        .expect("addr")
        .bind()
        .await
        .expect("bind server");
    let client = Endpoint::builder(presets::Minimal)
        .bind_addr("127.0.0.1:0")
        .expect("addr")
        .bind()
        .await
        .expect("bind client");

    let sock = server.bound_sockets()[0];
    let server_addr = EndpointAddr::from_parts(server.id(), [TransportAddr::Ip(sock)]);

    let serve = tokio::spawn(async move {
        let incoming = server.accept().await.expect("incoming");
        let conn = incoming.await.expect("accept");
        let mut send = conn.open_uni().await.expect("open_uni");
        let meta = StreamMeta {
            name: "test-agent".into(),
            started_at: "2026-06-30T00:00:00Z".into(),
            kind: "video only".into(),
            width: 320,
            height: 240,
            fps: 30,
        };
        write_meta(&mut send, &meta).await.unwrap();
        let mut s = video::TimecodeSource::new(320, 240, 30);
        for i in 0..3 {
            let jpeg = video::encode_jpeg(&s.render(i * 33), 70).unwrap();
            write_frame(&mut send, &jpeg).await.unwrap();
        }
        send.finish().ok();
        // Hold the connection open until the client has acknowledged the
        // finished stream, so dropping it can't reset undelivered data.
        send.stopped().await.ok();
    });

    let conn = client.connect(server_addr, ALPN).await.expect("connect");
    let mut recv = conn.accept_uni().await.expect("accept_uni");

    let meta = read_meta(&mut recv).await.expect("read_meta");
    assert_eq!(meta.name, "test-agent");
    assert_eq!(meta.kind, "video only");
    assert_eq!((meta.width, meta.height), (320, 240));

    let mut got = 0;
    while let Some(jpeg) = read_frame(&mut recv).await.expect("read") {
        let f = video::decode_jpeg(&jpeg).expect("decode");
        assert_eq!((f.width, f.height), (320, 240));
        got += 1;
    }
    assert_eq!(got, 3, "expected 3 streamed frames");

    drop(conn);
    serve.abort();
    let _ = serve.await;
}
