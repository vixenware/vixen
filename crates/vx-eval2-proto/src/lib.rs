use facet::Facet;

pub const EVAL2_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct Eval2Version {
    pub protocol_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct PlaceRef {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum ReuseClass {
    Stable,
    Observed,
    Live,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum CrossingPolicy {
    ByValue,
    ByProxy,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct RemoteRef {
    pub owner_place: String,
    pub handle_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum ProjectionStep {
    Field(String),
    Index(u64),
    MapKey(String),
    Path(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Facet)]
pub struct ByteOffset(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Facet)]
pub struct LineIndex(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Facet)]
pub struct ColumnIndex(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct SourcePoint {
    pub byte: ByteOffset,
    pub line: LineIndex,
    pub column: ColumnIndex,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct SourceSpan {
    pub source_id: String,
    pub start: SourcePoint,
    pub end: SourcePoint,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum RuntimeInputKind {
    Argument,
    AmbientEnv,
    Import,
    LocalPath,
    DiscoverResult,
    ExecResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum ProvenanceRoot {
    Unknown,
    Literal,
    Binding {
        name: String,
    },
    RuntimeInput {
        kind: RuntimeInputKind,
        label: String,
    },
    HostProbe {
        place: String,
        description: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct Provenance {
    pub source: SourceSpan,
    pub root: ProvenanceRoot,
    #[facet(default)]
    pub path: Vec<ProjectionStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum Claim {
    Os {
        value: String,
    },
    Arch {
        value: String,
    },
    Which {
        name: String,
        expected_path: Option<String>,
    },
    Bundle {
        name: String,
        identity: String,
    },
    Read {
        path: String,
        content_hash: String,
    },
    Hash {
        path: String,
        content_hash: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct ValueMeta {
    pub provenance: Provenance,
    #[facet(default)]
    pub claims: Vec<Claim>,
    pub reuse: ReuseClass,
    pub crossing: CrossingPolicy,
}

impl Provenance {
    pub fn unknown(source: SourceSpan) -> Self {
        Self {
            source,
            root: ProvenanceRoot::Unknown,
            path: Vec::new(),
        }
    }

    pub fn literal(source: SourceSpan) -> Self {
        Self {
            source,
            root: ProvenanceRoot::Literal,
            path: Vec::new(),
        }
    }

    pub fn binding(source: SourceSpan, name: impl Into<String>) -> Self {
        Self {
            source,
            root: ProvenanceRoot::Binding { name: name.into() },
            path: Vec::new(),
        }
    }

    pub fn runtime_input(
        source: SourceSpan,
        kind: RuntimeInputKind,
        label: impl Into<String>,
    ) -> Self {
        Self {
            source,
            root: ProvenanceRoot::RuntimeInput {
                kind,
                label: label.into(),
            },
            path: Vec::new(),
        }
    }

    pub fn host_probe(
        source: SourceSpan,
        place: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            source,
            root: ProvenanceRoot::HostProbe {
                place: place.into(),
                description: description.into(),
            },
            path: Vec::new(),
        }
    }

    pub fn project(&self, step: ProjectionStep) -> Self {
        let mut path = self.path.clone();
        path.push(step);
        Self {
            source: self.source.clone(),
            root: self.root.clone(),
            path,
        }
    }
}

impl SourcePoint {
    pub fn new(byte: ByteOffset, line: LineIndex, column: ColumnIndex) -> Self {
        Self { byte, line, column }
    }
}

impl SourceSpan {
    pub fn new(source_id: impl Into<String>, start: SourcePoint, end: SourcePoint) -> Self {
        Self {
            source_id: source_id.into(),
            start,
            end,
        }
    }

    pub fn synthetic(source_id: impl Into<String>) -> Self {
        let zero = SourcePoint::new(ByteOffset(0), LineIndex(0), ColumnIndex(0));
        Self::new(source_id, zero.clone(), zero)
    }
}

impl ValueMeta {
    pub fn new(provenance: Provenance, reuse: ReuseClass, crossing: CrossingPolicy) -> Self {
        Self {
            provenance,
            claims: Vec::new(),
            reuse,
            crossing,
        }
    }

    pub fn with_provenance(mut self, provenance: Provenance) -> Self {
        self.provenance = provenance;
        self
    }

    pub fn with_reuse(mut self, reuse: ReuseClass) -> Self {
        self.reuse = reuse;
        self
    }

    pub fn with_crossing(mut self, crossing: CrossingPolicy) -> Self {
        self.crossing = crossing;
        self
    }

    pub fn with_claim(mut self, claim: Claim) -> Self {
        if !self.claims.contains(&claim) {
            self.claims.push(claim);
        }
        self
    }

    pub fn with_claims(mut self, claims: impl IntoIterator<Item = Claim>) -> Self {
        for claim in claims {
            if !self.claims.contains(&claim) {
                self.claims.push(claim);
            }
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct StructValue {
    pub type_name: String,
    pub fields: Vec<(String, ValueEnvelope)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum WireValue {
    Unit(()),
    Bool(bool),
    U64(u64),
    I64(i64),
    String(String),
    Array(Vec<ValueEnvelope>),
    Map(Vec<(ValueEnvelope, ValueEnvelope)>),
    Struct(StructValue),
    External { kind: String, descriptor: String },
    Proxy(RemoteRef),
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ValueEnvelope {
    pub value: WireValue,
    pub meta: ValueMeta,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct EvalSubgraphRequest {
    pub place: PlaceRef,
    pub source_id: String,
    pub source: String,
    pub entry: String,
    #[facet(default)]
    pub args: Vec<ValueEnvelope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum EvalOutcome {
    Ready(ValueEnvelope),
    Poison { kind: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct VerifyClaimsRequest {
    pub place: PlaceRef,
    #[facet(default)]
    pub claims: Vec<Claim>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct VerifyClaimsResult {
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct FetchProxyRequest {
    pub requester_place: PlaceRef,
    pub reference: RemoteRef,
}

#[roam::service]
pub trait Eval2Daemon {
    async fn version(&self) -> Eval2Version;
    async fn eval_subgraph(&self, request: EvalSubgraphRequest) -> EvalOutcome;
    async fn verify_claims(&self, request: VerifyClaimsRequest) -> VerifyClaimsResult;
    async fn fetch_proxy(&self, request: FetchProxyRequest) -> EvalOutcome;
}

#[cfg(test)]
mod tests {
    use camino::Utf8Path;
    use moire::task::FutureExt;
    use roam_core::{BareConduit, acceptor};
    use roam_stream::StreamLink;
    use roam_types::MessageFamily;
    use tempfile::TempDir;
    use vx_io::net::{Listener, connect_session, default_local_endpoint, park_session};

    use super::*;

    #[test]
    fn eval_outcome_round_trips_via_postcard() {
        let wire_source = SourceSpan::synthetic("wire");
        let toolchain_source = SourceSpan::synthetic("toolchain");
        let value = EvalOutcome::Ready(ValueEnvelope {
            value: WireValue::Struct(StructValue {
                type_name: "Toolchain".to_string(),
                fields: vec![
                    (
                        "os".to_string(),
                        ValueEnvelope {
                            value: WireValue::String("windows".to_string()),
                            meta: ValueMeta::new(
                                Provenance::literal(wire_source.clone()),
                                ReuseClass::Stable,
                                CrossingPolicy::ByValue,
                            ),
                        },
                    ),
                    (
                        "cc".to_string(),
                        ValueEnvelope {
                            value: WireValue::String(
                                "C:/VS/VC/Tools/MSVC/14.3/bin/cl.exe".to_string(),
                            ),
                            meta: ValueMeta::new(
                                Provenance::host_probe(
                                    toolchain_source.clone(),
                                    "win",
                                    "toolchain",
                                ),
                                ReuseClass::Observed,
                                CrossingPolicy::ByValue,
                            )
                            .with_claim(Claim::Which {
                                name: "cl.exe".to_string(),
                                expected_path: Some(
                                    "C:/VS/VC/Tools/MSVC/14.3/bin/cl.exe".to_string(),
                                ),
                            }),
                        },
                    ),
                ],
            }),
            meta: ValueMeta {
                provenance: Provenance {
                    source: toolchain_source,
                    root: ProvenanceRoot::HostProbe {
                        place: "win".to_string(),
                        description: "toolchain".to_string(),
                    },
                    path: vec![ProjectionStep::Field("cc".to_string())],
                },
                claims: vec![Claim::Which {
                    name: "cl.exe".to_string(),
                    expected_path: Some("C:/VS/VC/Tools/MSVC/14.3/bin/cl.exe".to_string()),
                }],
                reuse: ReuseClass::Observed,
                crossing: CrossingPolicy::ByValue,
            },
        });

        let bytes = facet_postcard::to_vec(&value).expect("serialize eval outcome");
        let decoded: EvalOutcome =
            facet_postcard::from_slice(&bytes).expect("deserialize eval outcome");
        assert_eq!(decoded, value);
    }

    #[derive(Clone)]
    struct TestDaemon;

    impl Eval2Daemon for TestDaemon {
        async fn version(&self) -> Eval2Version {
            Eval2Version {
                protocol_version: EVAL2_PROTOCOL_VERSION,
            }
        }

        async fn eval_subgraph(&self, request: EvalSubgraphRequest) -> EvalOutcome {
            EvalOutcome::Ready(ValueEnvelope {
                value: WireValue::String(format!("{}::{}", request.place.id, request.entry)),
                meta: ValueMeta {
                    provenance: Provenance {
                        source: SourceSpan::synthetic(request.source_id),
                        root: ProvenanceRoot::RuntimeInput {
                            kind: RuntimeInputKind::ExecResult,
                            label: "rpc".to_string(),
                        },
                        path: Vec::new(),
                    },
                    claims: Vec::new(),
                    reuse: ReuseClass::Stable,
                    crossing: CrossingPolicy::ByValue,
                },
            })
        }

        async fn verify_claims(&self, request: VerifyClaimsRequest) -> VerifyClaimsResult {
            VerifyClaimsResult {
                ok: request.place.id == "win" && !request.claims.is_empty(),
                error: None,
            }
        }

        async fn fetch_proxy(&self, request: FetchProxyRequest) -> EvalOutcome {
            EvalOutcome::Ready(ValueEnvelope {
                value: WireValue::Proxy(request.reference),
                meta: ValueMeta {
                    provenance: Provenance {
                        source: SourceSpan::synthetic("fetch-proxy"),
                        root: ProvenanceRoot::Unknown,
                        path: Vec::new(),
                    },
                    claims: Vec::new(),
                    reuse: ReuseClass::Live,
                    crossing: CrossingPolicy::ByProxy,
                },
            })
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn roam_local_round_trip_uses_real_eval2_daemon_service() {
        let root = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(root.path()).expect("utf8 tempdir");
        let endpoint = default_local_endpoint(root, "vx-eval2-proto-test");
        let mut listener = Listener::bind(&endpoint)
            .await
            .expect("bind local listener");
        let daemon = TestDaemon;

        let accept_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept local connection");
            let daemon = daemon.clone();
            moire::task::spawn(
                async move {
                    let (reader, writer) = tokio::io::split(stream);
                    let conduit =
                        BareConduit::<MessageFamily, _>::new(StreamLink::new(reader, writer));
                    let dispatcher = Eval2DaemonDispatcher::new(daemon);
                    let (caller, session_handle) = acceptor(conduit)
                        .establish::<roam_core::NoopCaller>(dispatcher)
                        .await
                        .expect("roam session establish");
                    park_session(caller, session_handle).await;
                }
                .named("vx_eval2_proto_test_connection"),
            );
        });

        let (caller, session_handle) = connect_session(&endpoint)
            .await
            .expect("connect roam session");
        let client = Eval2DaemonClient::new(caller.clone());

        let version = client.version().await.expect("rpc version");
        assert_eq!(version.protocol_version, EVAL2_PROTOCOL_VERSION);

        let outcome = client
            .eval_subgraph(EvalSubgraphRequest {
                place: PlaceRef {
                    id: "win".to_string(),
                },
                source_id: "rpc-test".to_string(),
                source: "fn build() { 42 }".to_string(),
                entry: "build".to_string(),
                args: Vec::new(),
            })
            .await
            .expect("rpc eval_subgraph");
        assert_eq!(
            outcome,
            EvalOutcome::Ready(ValueEnvelope {
                value: WireValue::String("win::build".to_string()),
                meta: ValueMeta {
                    provenance: Provenance {
                        source: SourceSpan::synthetic("rpc-test"),
                        root: ProvenanceRoot::RuntimeInput {
                            kind: RuntimeInputKind::ExecResult,
                            label: "rpc".to_string(),
                        },
                        path: Vec::new(),
                    },
                    claims: Vec::new(),
                    reuse: ReuseClass::Stable,
                    crossing: CrossingPolicy::ByValue,
                },
            })
        );

        let verified = client
            .verify_claims(VerifyClaimsRequest {
                place: PlaceRef {
                    id: "win".to_string(),
                },
                claims: vec![Claim::Which {
                    name: "cl.exe".to_string(),
                    expected_path: Some("C:/VS/VC/Tools/MSVC/14.3/bin/cl.exe".to_string()),
                }],
            })
            .await
            .expect("rpc verify_claims");
        assert!(verified.ok);

        drop(client);
        drop(caller);
        drop(session_handle);
        accept_task.abort();
    }
}
