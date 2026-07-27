#[tokio::main]
async fn main() {
    let scratch = std::env::temp_dir().join("kedge-adv-report");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let mut scores = Vec::new();
    for d in [
        kedge_bench::Defence::None,
        kedge_bench::Defence::SkillManifest,
        kedge_bench::Defence::DenyAll,
    ] {
        scores.push(kedge_bench::run_security(d, &scratch).await.unwrap());
    }
    println!("{}", kedge_bench::security_report(&scores));
}
