use super::*;
use facet_value::value;

#[test]
fn parse_invoke_spec_with_host_executor() {
    let source = r#"
        entry build
        capabilities (fs discover)

        runtime {
            executors {
                default @host{
                    os windows
                    arch x86_64
                    which (
                        {name "cl.exe", path "C:/VS/cl.exe"}
                        {name "lib.exe", path "C:/VS/lib.exe"}
                    )
                }
            }
        }
    "#;

    let spec = InvokeSpec::parse(source).unwrap();
    assert_eq!(spec.entry, "build");
    assert_eq!(spec.capabilities, vec!["fs", "discover"]);
    assert_eq!(spec.runtime.executors.len(), 1);
    assert_eq!(
        spec.runtime.executors.get("default"),
        Some(&ExecutorSpec::Host(HostSpec {
            os: "windows".to_string(),
            arch: "x86_64".to_string(),
            which: vec![
                WhichEntry {
                    name: "cl.exe".to_string(),
                    path: "C:/VS/cl.exe".to_string(),
                },
                WhichEntry {
                    name: "lib.exe".to_string(),
                    path: "C:/VS/lib.exe".to_string(),
                },
            ],
            reads: Vec::new(),
            hashes: Vec::new(),
        }))
    );
}

#[test]
fn parse_expect_spec_with_event_assertion_list() {
    let source = r#"
        result {
            type Toolchain
            fields {
                os windows
                arch x86_64
            }
        }

        obligations (
            @os{value windows}
            @arch{value x86_64}
        )

        events (
            @must_emit(@discover_start{executor default})
            @must_emit(@discover_end)
            @must_not_emit(@exec_start)
            @before{
                first @discover_start{executor default}
                then @discover_end
            }
        )
    "#;

    let spec = ExpectSpec::parse(source).unwrap();
    assert_eq!(
        spec.result,
        Some(value!({
            "type": "Toolchain",
            "fields": {
                "os": "windows",
                "arch": "x86_64",
            }
        }))
    );
    assert_eq!(
        spec.obligations,
        vec![
            ObservationPattern::Os {
                value: "windows".to_string(),
            },
            ObservationPattern::Arch {
                value: "x86_64".to_string(),
            },
        ]
    );
    assert_eq!(
        spec.events,
        vec![
            EventAssertion::MustEmit(vec![EventPattern::DiscoverStart {
                executor: "default".to_string(),
            }]),
            EventAssertion::MustEmit(vec![EventPattern::DiscoverEnd]),
            EventAssertion::MustNotEmit(vec![EventPattern::ExecStart]),
            EventAssertion::Before(OrderConstraint {
                first: EventPattern::DiscoverStart {
                    executor: "default".to_string(),
                },
                then: EventPattern::DiscoverEnd,
            }),
        ]
    );
}
