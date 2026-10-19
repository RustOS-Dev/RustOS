//! tests/fixtures/constructs.asl through the interpreter.

use acpi::aml::object::Object;
use aml_check::{Machine, eval, interpreter, load, prt_routes};
use std::str::FromStr;

fn setup() -> (Machine, acpi::aml::Interpreter<Machine>) {
    let table = include_bytes!("fixtures/constructs.aml");
    let machine = Machine::new();
    let interp = interpreter(&machine, None, table[8]);
    load(&interp, table).expect("constructs.aml loads");
    (machine, interp)
}

fn int(o: Object) -> u64 {
    match o {
        Object::Integer(v) => v,
        o => panic!("not an integer: {:?}", o.typ()),
    }
}

#[test]
fn index_field_read_at_load_time() {
    // Bytes 8..11 of the 0x72/0x73 table are 0x18..0x1b.
    let (_, interp) = setup();
    assert_eq!(int(eval(&interp, "\\IXRD").unwrap()), 0x1b1a_1918);
}

#[test]
fn index_field_write() {
    let (machine, interp) = setup();
    let name = acpi::aml::namespace::AmlName::from_str("\\IXWR").unwrap();
    let r = interp
        .evaluate(name, vec![Object::Integer(0xaabb_ccdd).wrap()])
        .unwrap();
    assert_eq!(int((*r).clone()), 0xaabb_ccdd);
    assert_eq!(
        machine.0.lock().unwrap().table[8..12],
        [0xdd, 0xcc, 0xbb, 0xaa]
    );
}

#[test]
fn bank_field_selects_bank() {
    let (machine, interp) = setup();
    let name = acpi::aml::namespace::AmlName::from_str("\\BKWR").unwrap();
    interp
        .evaluate(name, vec![Object::Integer(0x5a).wrap()])
        .unwrap();
    let s = machine.0.lock().unwrap();
    assert_eq!((s.io[0x80], s.io[0x90]), (1, 0x5a));
}

#[test]
fn deref_of_buffer_element() {
    let (_, interp) = setup();
    let name = acpi::aml::namespace::AmlName::from_str("\\DIDX").unwrap();
    let r = interp
        .evaluate(name, vec![Object::Integer(2).wrap()])
        .unwrap();
    assert_eq!(int((*r).clone()), 0x33);
}

#[test]
fn deref_of_reference_and_field_unit() {
    let (_, interp) = setup();
    assert_eq!(int(eval(&interp, "\\DREF").unwrap()), 0x1b1a_1918);
    // CDAT reads the table at the index last written (11, after IXRD).
    assert_eq!(int(eval(&interp, "\\DFLD").unwrap()), 0x1b);
}

#[test]
fn buffer_fields_as_operands() {
    let (_, interp) = setup();
    assert_eq!(int(eval(&interp, "\\BFDC").unwrap()), 0x0f);
    assert_eq!(int(eval(&interp, "\\BFIN").unwrap()), 6);
    match eval(&interp, "\\BFEQ").unwrap() {
        Object::Buffer(b) => assert_eq!(&b[..2], &[0x10, 0]),
        o => panic!("BFEQ gave {:?}", o.typ()),
    }
}

#[test]
fn bridge_prt_routes() {
    use acpi::aml::namespace::AmlName;
    use acpi::aml::pci_routing::{PciRoutingTable, Pin};
    let (_, interp) = setup();
    assert_eq!(prt_routes(&interp, "\\GPPA._PRT").unwrap(), Some(2));
    assert_eq!(prt_routes(&interp, "\\LNKA._PRT").unwrap(), None);
    let t =
        PciRoutingTable::from_prt_path(AmlName::from_str("\\GPPA._PRT").unwrap(), &interp).unwrap();
    assert_eq!(t.route(0, 0, Pin::IntA, &interp).unwrap().irq, 0x1c);
    assert_eq!(t.route(0, 1, Pin::IntB, &interp).unwrap().irq, 0x1e);
}
