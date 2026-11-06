use super::*;
#[test]
fn all_supported_wire_types_round_trip_and_reject_bad_tails() {
    let bytes = Pb::new().int(1, u64::MAX).text(2, "中文").double(3, 0.5).0;
    let values = fields(&bytes).unwrap();
    assert_eq!(values[0].number, u64::MAX);
    assert_eq!(values[1].text().unwrap(), "中文");
    assert_eq!(f64::from_le_bytes(values[2].data.try_into().unwrap()), 0.5);
    for tail in [vec![0], vec![8, 128], vec![18, 2, 1], vec![9, 1], vec![15]] {
        assert!(fields(&tail).is_err());
    }
}
#[test]
fn gzip_threshold_matches_native_cli_and_remains_bounded() {
    let small = frame(&vec![0; 8192]).unwrap();
    assert_eq!(small[0], 0);
    let large = frame(&vec![b'x'; 8193]).unwrap();
    assert_eq!(large[0], 1);
    assert_eq!(inflate(&large[5..]).unwrap(), vec![b'x'; 8193]);
    assert!(large.len() < 8193);
}
#[test]
fn overflowing_varints_and_invalid_utf8_are_refused() {
    let mut data = vec![8];
    data.extend_from_slice(&[255; 10]);
    assert!(fields(&data).is_err());
    let fields = fields(&[10, 1, 255]).unwrap();
    assert!(fields[0].text().is_err());
}
