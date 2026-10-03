//! 电子表格里的日期格读成它显示的日期，不是 Excel 的序数。
//!
//! 照 Excel 写出来的样子拼一份 xlsx：日期存成从 1899-12-30 起的天数（`<v>45306</v>`），
//! 小数部分是一天里的时刻；它是日期，只因为 styles.xml 里挂的数字格式是日期格式——内置的
//! 14（短日期）、22（日期加时刻）、20（时刻）、46（累计时长），和中文 Excel 自定义的
//! `yyyy"年"m"月"d"日"`。

use std::io::{Cursor, Write};

// cellXfs by index: 0 General, 1 short date (14), 2 a Chinese custom date (176),
// 3 date and time (22), 4 time of day (20), 5 elapsed time (46), 6 "#,##0.00" (4).
const STYLES: &str = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><numFmts count="1"><numFmt numFmtId="176" formatCode="yyyy&quot;年&quot;m&quot;月&quot;d&quot;日&quot;"/></numFmts><cellXfs count="7"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/><xf numFmtId="14" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/><xf numFmtId="176" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/><xf numFmtId="22" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/><xf numFmtId="20" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/><xf numFmtId="46" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/><xf numFmtId="4" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/></cellXfs></styleSheet>"#;

fn workbook(rows: &[Vec<String>], date1904: bool) -> Vec<u8> {
    workbook_with_styles(rows, date1904, STYLES)
}

fn workbook_with_styles(rows: &[Vec<String>], date1904: bool, styles: &str) -> Vec<u8> {
    let sheet_data: String = rows
        .iter()
        .enumerate()
        .map(|(i, cells)| format!(r#"<row r="{}">{}</row>"#, i + 1, cells.concat()))
        .collect();
    let workbook_pr = if date1904 {
        r#"<workbookPr date1904="1"/>"#
    } else {
        "<workbookPr/>"
    };
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (part, xml) in [
        ("[Content_Types].xml", r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#.to_string()),
        ("_rels/.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.to_string()),
        ("xl/workbook.xml", format!(r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">{workbook_pr}<sheets><sheet name="Ledger" sheetId="1" r:id="rId1"/></sheets></workbook>"#)),
        ("xl/_rels/workbook.xml.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#.to_string()),
        ("xl/styles.xml", styles.to_string()),
        ("xl/worksheets/sheet1.xml", format!(r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{sheet_data}</sheetData></worksheet>"#)),
    ] {
        zip.start_file(part, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn read_xlsx_format(format_id: u16, value: &str, custom: Option<&str>, date1904: bool) -> String {
    let custom_format = custom.map_or_else(String::new, |code| {
        let code = quick_xml::escape::escape(code);
        format!(
            r#"<numFmts count="1"><numFmt numFmtId="{format_id}" formatCode="{code}"/></numFmts>"#
        )
    });
    let styles = format!(
        r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">{custom_format}<cellXfs count="1"><xf numFmtId="{format_id}" applyNumberFormat="1"/></cellXfs></styleSheet>"#
    );
    let rows = [
        header(1),
        vec![
            words("A2", "Value"),
            number("B2", 0, value),
            words("C2", "ok"),
        ],
    ];
    utopia_ingest::parse(
        "ledger.xlsx",
        &workbook_with_styles(&rows, date1904, &styles),
    )
    .unwrap()
    .text
}

fn assert_value(text: &str, expected: &str) {
    assert!(text.contains(&format!("| Value | {expected} |")), "{text}");
}

// A BIFF8 Workbook stream inside an actual Compound File exercises the public
// .xls reader, including XF/FORMAT resolution, rather than the format helper alone.
fn read_xls_format(format_id: u16, value: f64, custom: Option<&str>, date1904: bool) -> String {
    read_xls_numeric_record(format_id, value, custom, date1904, XlsNumericRecord::Number)
}

#[derive(Clone, Copy)]
enum XlsNumericRecord {
    Number,
    Rk,
    MulRk,
    Formula,
}

fn read_xls_numeric_record(
    format_id: u16,
    value: f64,
    custom: Option<&str>,
    date1904: bool,
    numeric_record: XlsNumericRecord,
) -> String {
    fn record(stream: &mut Vec<u8>, record_id: u16, data: &[u8]) {
        stream.extend_from_slice(&record_id.to_le_bytes());
        stream.extend_from_slice(&(data.len() as u16).to_le_bytes());
        stream.extend_from_slice(data);
    }

    fn bof(stream: &mut Vec<u8>, substream_type: u16) {
        let mut data = [0; 16];
        data[..2].copy_from_slice(&0x0600u16.to_le_bytes());
        data[2..4].copy_from_slice(&substream_type.to_le_bytes());
        record(stream, 0x0809, &data);
    }

    fn label(stream: &mut Vec<u8>, row: u16, col: u16, text: &str) {
        let mut data = Vec::new();
        data.extend_from_slice(&row.to_le_bytes());
        data.extend_from_slice(&col.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes()); // XF index
        data.extend_from_slice(&(text.len() as u16).to_le_bytes());
        data.push(0); // compressed Unicode: these labels are ASCII
        data.extend_from_slice(text.as_bytes());
        record(stream, 0x0204, &data);
    }

    let mut stream = Vec::new();
    bof(&mut stream, 0x0005); // workbook globals
    record(&mut stream, 0x0022, &u16::from(date1904).to_le_bytes());
    if let Some(code) = custom {
        let code: Vec<u16> = code.encode_utf16().collect();
        let mut data = Vec::new();
        data.extend_from_slice(&format_id.to_le_bytes());
        data.extend_from_slice(&(code.len() as u16).to_le_bytes());
        data.push(1); // UTF-16 FORMAT string
        for character in code {
            data.extend_from_slice(&character.to_le_bytes());
        }
        record(&mut stream, 0x041E, &data);
    }
    let mut xf = [0; 20];
    xf[2..4].copy_from_slice(&format_id.to_le_bytes());
    record(&mut stream, 0x00E0, &xf);
    let boundsheet_position = stream.len();
    let mut boundsheet = vec![0; 8];
    boundsheet[6] = 6;
    boundsheet.extend_from_slice(b"Ledger");
    record(&mut stream, 0x0085, &boundsheet);
    record(&mut stream, 0x000A, &[]);
    let sheet_position = stream.len() as u32;
    stream[boundsheet_position + 4..boundsheet_position + 8]
        .copy_from_slice(&sheet_position.to_le_bytes());

    bof(&mut stream, 0x0010); // worksheet
    label(&mut stream, 0, 0, "Item");
    label(&mut stream, 0, 1, "When");
    label(&mut stream, 1, 0, "Value");
    let mut number = Vec::new();
    number.extend_from_slice(&1u16.to_le_bytes()); // row
    number.extend_from_slice(&1u16.to_le_bytes()); // column
    number.extend_from_slice(&0u16.to_le_bytes()); // XF index
    match numeric_record {
        XlsNumericRecord::Number => {
            number.extend_from_slice(&value.to_le_bytes());
            record(&mut stream, 0x0203, &number);
        }
        XlsNumericRecord::Rk | XlsNumericRecord::MulRk => {
            // RK stores this fixture's integer in the upper 30 bits; bit 1 is the integer flag.
            assert_eq!(value.fract(), 0.0);
            let encoded_value = ((value as i32) << 2) | 2;
            number.extend_from_slice(&encoded_value.to_le_bytes());
            if matches!(numeric_record, XlsNumericRecord::MulRk) {
                number.extend_from_slice(&1u16.to_le_bytes()); // last column
                record(&mut stream, 0x00BD, &number);
            } else {
                record(&mut stream, 0x027E, &number);
            }
        }
        XlsNumericRecord::Formula => {
            number.extend_from_slice(&value.to_le_bytes()); // cached numeric result
            number.extend_from_slice(&[0; 8]); // flags, chain and empty formula tokens
            record(&mut stream, 0x0006, &number);
        }
    }
    record(&mut stream, 0x000A, &[]);

    let mut compound = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
    compound
        .create_stream("/Workbook")
        .unwrap()
        .write_all(&stream)
        .unwrap();
    let bytes = compound.into_inner().into_inner();
    utopia_ingest::parse("ledger.xls", &bytes).unwrap().text
}

fn read(rows: &[Vec<String>], date1904: bool) -> String {
    utopia_ingest::parse("ledger.xlsx", &workbook(rows, date1904))
        .unwrap()
        .text
}

fn words(cell: &str, text: &str) -> String {
    format!(r#"<c r="{cell}" t="inlineStr"><is><t>{text}</t></is></c>"#)
}

fn number(cell: &str, style: u8, value: &str) -> String {
    format!(r#"<c r="{cell}" s="{style}"><v>{value}</v></c>"#)
}

fn header(row: u32) -> Vec<String> {
    vec![
        words(&format!("A{row}"), "Item"),
        words(&format!("B{row}"), "When"),
        words(&format!("C{row}"), "Amount"),
    ]
}

#[test]
fn a_date_cell_reads_as_the_date_it_shows() {
    let text = read(
        &[
            header(1),
            vec![
                words("A2", "Short date"),
                number("B2", 1, "45306"),
                number("C2", 6, "1200"),
            ],
            vec![
                words("A3", "Chinese date"),
                number("B3", 2, "45307"),
                number("C3", 6, "800"),
            ],
            // 没挂日期格式的数就是数，哪怕它碰巧落在日期的范围里
            vec![
                words("A4", "Plain number"),
                number("B4", 0, "45306"),
                number("C4", 6, "5"),
            ],
        ],
        false,
    );
    assert!(
        text.contains("| Short date | 2024-01-15 | 1200 |"),
        "{text}"
    );
    assert!(
        text.contains("| Chinese date | 2024-01-16 | 800 |"),
        "{text}"
    );
    assert!(text.contains("| Plain number | 45306 | 5 |"), "{text}");
}

#[test]
fn a_time_of_day_and_an_elapsed_time_read_as_clock_time() {
    let text = read(
        &[
            header(1),
            vec![
                words("A2", "Opened"),
                number("B2", 3, "45306.395833333336"),
                number("C2", 6, "1"),
            ],
            vec![
                words("A3", "Daily call"),
                number("B3", 4, "0.39583333333333331"),
                number("C3", 6, "2"),
            ],
            vec![
                words("A4", "Downtime"),
                number("B4", 5, "1.5104166666666667"),
                number("C4", 6, "3"),
            ],
        ],
        false,
    );
    assert!(text.contains("| Opened | 2024-01-15 09:30 | 1 |"), "{text}");
    assert!(text.contains("| Daily call | 09:30 | 2 |"), "{text}");
    assert!(text.contains("| Downtime | 36:15:00 | 3 |"), "{text}");
}

#[test]
fn a_workbook_counting_from_1904_reads_the_same_date() {
    // Excel for Mac 从前默认 1904 纪年：同一天存成少 1462 的数
    let text = read(
        &[
            header(1),
            vec![
                words("A2", "Short date"),
                number("B2", 1, "43844"),
                number("C2", 6, "1200"),
            ],
        ],
        true,
    );
    assert!(
        text.contains("| Short date | 2024-01-15 | 1200 |"),
        "{text}"
    );
}

#[test]
fn xlsx_east_asian_builtin_dates_need_no_custom_format_definition() {
    // These IDs are dates across the East Asian locale tables. They have no
    // numFmt element in the xlsx fixture and no FORMAT record in the xls fixture.
    for format_id in [27, 28, 29, 30, 31, 36, 50, 51, 54, 57, 58] {
        assert_value(
            &read_xlsx_format(format_id, "45306", None, false),
            "2024-01-15",
        );
    }
}

#[test]
fn xls_east_asian_builtin_dates_need_no_custom_format_definition() {
    for format_id in [27, 28, 29, 30, 31, 36, 50, 51, 54, 57, 58] {
        assert_value(
            &read_xls_format(format_id, 45306.0, None, false),
            "2024-01-15",
        );
    }
}

const EAST_ASIAN_TIME_CASES: [(&str, &str); 4] = [
    ("0.39583333333333331", "09:30"),
    ("45306.395833333336", "09:30"),
    ("45306.395844907405", "09:30:01"),
    ("45306", "00:00"),
];

#[test]
fn xlsx_east_asian_builtin_times_do_not_gain_a_calendar_date() {
    for format_id in [32, 33] {
        for (value, expected) in EAST_ASIAN_TIME_CASES {
            assert_value(&read_xlsx_format(format_id, value, None, false), expected);
        }
    }
}

#[test]
fn xls_east_asian_builtin_times_do_not_gain_a_calendar_date() {
    for format_id in [32, 33] {
        for (value, expected) in EAST_ASIAN_TIME_CASES {
            assert_value(
                &read_xls_format(format_id, value.parse().unwrap(), None, false),
                expected,
            );
        }
    }
}

#[test]
fn locale_dependent_builtin_formats_are_not_guessed_from_the_id() {
    // These IDs switch between date and clock formats by locale; without a
    // formatCode or workbook locale there is insufficient evidence to choose.
    for format_id in [34, 35, 52, 53, 55, 56] {
        assert_value(&read_xlsx_format(format_id, "45306", None, false), "45306");
        assert_value(&read_xls_format(format_id, 45306.0, None, false), "45306");
    }
}

#[test]
fn xlsx_east_asian_builtin_dates_respect_the_1904_epoch() {
    for format_id in [27, 31, 50, 58] {
        assert_value(
            &read_xlsx_format(format_id, "43844", None, true),
            "2024-01-15",
        );
    }
}

#[test]
fn xls_east_asian_builtin_dates_respect_the_1904_epoch() {
    for format_id in [27, 31, 50, 58] {
        assert_value(
            &read_xls_format(format_id, 43844.0, None, true),
            "2024-01-15",
        );
    }
}

#[test]
fn explicit_xlsx_format_codes_override_builtin_id_classification() {
    for (format_id, code, value, expected) in [
        (27, "0.00", "45306", "45306"),
        (32, "yyyy-mm-dd", "45306", "2024-01-15"),
        (27, "[h]:mm:ss", "1.5104166666666667", "36:15:00"),
        (34, "yyyy-mm-dd", "45306", "2024-01-15"),
    ] {
        assert_value(
            &read_xlsx_format(format_id, value, Some(code), false),
            expected,
        );
    }
}

#[test]
fn xls_general_standard_custom_datetime_and_duration_formats_stay_correct() {
    for (format_id, code, value, expected) in [
        (0, None, 45306.0, "45306"),
        (14, None, 45306.0, "2024-01-15"),
        (176, Some("yyyy\"年\"m\"月\"d\"日\""), 45306.0, "2024-01-15"),
        (22, None, 45306.395833333336, "2024-01-15 09:30"),
        (20, None, 0.395_833_333_333_333_3, "09:30"),
        (46, None, 1.5104166666666667, "36:15:00"),
    ] {
        assert_value(&read_xls_format(format_id, value, code, false), expected);
    }
}

#[test]
fn xls_integer_records_and_formula_caches_use_the_cell_format() {
    for record in [
        XlsNumericRecord::Rk,
        XlsNumericRecord::MulRk,
        XlsNumericRecord::Formula,
    ] {
        for (format_id, expected) in [(27, "2024-01-15"), (32, "00:00"), (0, "45306")] {
            assert_value(
                &read_xls_numeric_record(format_id, 45306.0, None, false, record),
                expected,
            );
        }
    }
}
