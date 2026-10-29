use super::*;

struct NoNetwork;
#[async_trait]
impl HttpClient for NoNetwork {
    async fn stream(&self,_:HttpRequest)->Result<HttpStreamingResponse,HttpClientError>{panic!("Disabled accounts must not reach the network")}
}
#[tokio::test]
async fn disabled_source_helpers_child(){
    if env::var_os("NORA_POLICY_CHILD").is_none(){return;}
    let absent=Path::new("/nora-fixture-never-created/credentials.json");
    assert_eq!(codex_source::bind(absent).unwrap_err(),codex_source::SourceError::InvalidSource);
    assert_eq!(copilot_editors::take_over(absent,&NoNetwork).await.unwrap_err(),copilot_editors::EditorsError::InvalidSource);
    assert_eq!(copilot_editors::discover(absent,&BTreeMap::new(),&NoNetwork).await.unwrap_err(),copilot_editors::EditorsError::InvalidSource);
    let error=grok_source::bind_current_account_with_network(&NoNetwork,None).await.unwrap_err();assert!(error.contains("disabled"));
    let error=grok_source::discover_current_account_with_network(&NoNetwork,None).await.unwrap_err();assert!(error.contains("disabled"));
}
#[test]
fn disabled_source_helpers_stop_before_reading_local_credentials(){
    let output=std::process::Command::new(env::current_exe().unwrap()).args(["--exact","subscription_policy_test::disabled_source_helpers_child"]).env("NORA_POLICY_CHILD","1").env("NORA_NO_ACCOUNTS","true").output().unwrap();
    assert!(output.status.success(),"{}\n{}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));
}
