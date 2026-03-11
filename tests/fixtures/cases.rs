#[tokio::test]
async fn test_all_cases() {
    struct TestCase {
        name: &'static str,
        input: &'static str,
        output: &'static str,
    }

    let cases = &[
        TestCase {
            name: "Withdraw without money fails silently, deposit, withdraw excessive rejected silently, withdraw allowed accepted",
            input: include!("01_input.csv"),
            output: include!("01_output.csv"),
        },
        TestCase {
            name: "Withdraw without money fails silently, deposit, withdraw excessive rejected silently, withdraw allowed accepted",
            input: include!("02_input.csv"),
            output: include!("02_output.csv"),
        },
    ];

    for case in cases {
        let mut input = Vec::new();
        let mut reader = csv::Reader::from_reader(case.input.as_bytes());
    }
}
