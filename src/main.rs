use futures::stream::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::process;
use std::time::Duration;
use structopt::StructOpt;

#[derive(StructOpt, Debug)]
#[structopt(
    name = "askJira",
    about = "Ask Cody a question or a question about Jira tickets if JQL is provided."
)]
struct Opt {
    #[structopt(long, required = true, help = "The message to send to Cody")]
    message: String,

    #[structopt(long, help = "JQL query to search Jira tickets")]
    jql: Option<String>,

    #[structopt(long, default_value = "50", help = "Maximum number of issues to fetch")]
    max_issues: usize,

    #[structopt(long, help = "Enable debug mode")]
    debug: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().collect();

    if args.len() == 1 {
        Opt::clap().print_help().unwrap();
        println!();
        process::exit(0);
    }

    let opt = Opt::from_args();

    let access_token = env::var("SRC_ACCESS_TOKEN")
        .expect("Error: SRC_ACCESS_TOKEN environment variable is not set.");
    let endpoint =
        env::var("SRC_ENDPOINT").expect("Error: SRC_ENDPOINT environment variable is not set.");

    let chat_completions_url = format!(
        "{}/.api/completions/stream?api-version=1&client-name=jetbrains&client-version=6.0.0-SNAPSHOT'",
        endpoint
    );

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("token {}", access_token))?,
    );

    if let Some(jql) = opt.jql {
        let jira_token = env::var("JIRA_TOKEN").map_err(|_| "JIRA_TOKEN environment variable is not set")?;
        let jira_host = env::var("JIRA_HOST").map_err(|_| "JIRA_HOST environment variable is not set")?;

        if opt.debug {
            println!("DEBUG: JIRA_HOST = {}", jira_host);
            println!("DEBUG: JQL Query = {:?}", jql);
        }

        let jira_data =
            fetch_jira_data(&jira_host, &jira_token, &jql, opt.max_issues, opt.debug).await?;
        let batch_summaries = process_jira_data(
            &opt.message,
            jira_data,
            &chat_completions_url,
            &headers,
            opt.debug,
        )
        .await?;

        let aggregated_data = batch_summaries.join("\n\n");

        let final_query = format!(
            "Original question(s):\n{}\n\nAggregated Jira data:\n{}\n\nBased on aggregated Jira data, please provide a comprehensive answer for the original question(s).",
            opt.message,
            aggregated_data
        );

        let final_answer =
            cody_chat(&final_query, &chat_completions_url, &headers, opt.debug).await?;
        println!("Answer:\n{}", final_answer);
    } else {
        let answer = cody_chat(&opt.message, &chat_completions_url, &headers, opt.debug).await?;
        println!("Answer: {}", answer);
    }

    Ok(())
}

async fn fetch_jira_data(
    host: &str,
    token: &str,
    jql: &str,
    max_issues: usize,
    debug: bool,
) -> Result<String, Box<dyn Error>> {
    let client = Client::builder()
        .timeout(Duration::from_secs(300))
        .build()?;
    let search_url = format!("{}/rest/api/2/search", host);

    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", token))?,
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    if debug {
        println!("DEBUG: Sending search request to {}", search_url);
    }

    let mut start_at = 0;
    let max_results = 50;
    let mut all_issues = Vec::new();

    loop {
        let search_response = client
            .get(&search_url)
            .headers(headers.clone())
            .query(&[
                ("jql", jql),
                ("startAt", &start_at.to_string()),
                ("maxResults", &max_results.to_string()),
            ])
            .send()
            .await?;

        if debug {
            println!(
                "DEBUG: Search response status: {}",
                search_response.status()
            );
        }

        let search_status = search_response.status();
        let search_body = search_response.text().await?;

        if !search_status.is_success() {
            if debug {
                println!("DEBUG: Search error response body: {}", search_body);
            }
            return Err(format!(
                "Jira search API request failed with status: {}. Body: {}",
                search_status, search_body
            )
            .into());
        }

        let search_data: Value = serde_json::from_str(&search_body)?;
        let issues = search_data["issues"].as_array().ok_or("No issues found")?;

        all_issues.extend_from_slice(issues);

        let total = search_data["total"].as_u64().unwrap_or(0) as usize;
        start_at += issues.len();

        if debug {
            println!(
                "DEBUG: Fetched {} issues out of {}",
                all_issues.len(),
                total
            );
        }

        if start_at >= total || all_issues.len() >= max_issues {
            break;
        }
    }

    if debug {
        println!(
            "DEBUG: Total number of issues fetched: {}",
            all_issues.len()
        );
    }

    let mut aggregated_data = Vec::new();
    let mut aggregated_count = 0;

    for issue in all_issues.iter().take(max_issues) {
        let key = issue["key"].as_str().ok_or("Issue key not found")?;

        let comments_url = format!("{}/rest/api/2/issue/{}/comment", host, key);
        let comments_response = client
            .get(&comments_url)
            .headers(headers.clone())
            .send()
            .await?;

        let comments_body = comments_response.text().await?;
        let comments_data: Value = serde_json::from_str(&comments_body)?;

        let mut issue_data = json!({
            "key": key,
            "fields": issue["fields"],
            "comments": comments_data["comments"]
        });

        remove_null_values(&mut issue_data);

        aggregated_data.push(issue_data);
        aggregated_count += 1;
        if debug {
            println!(
                "DEBUG: Aggregated issue {}/{}",
                aggregated_count, max_issues
            );
        }
    }

    if debug {
        println!("DEBUG: Total issues aggregated: {}", aggregated_count);
    }

    let result = serde_json::to_string_pretty(&aggregated_data)?;
    if debug {
        println!("DEBUG: Aggregated data length: {} characters", result.len());
    }

    Ok(result)
}

fn remove_null_values(value: &mut Value) {
    if let Value::Object(map) = value {
        map.retain(|_, v| !v.is_null());
        for v in map.values_mut() {
            remove_null_values(v);
        }
    } else if let Value::Array(array) = value {
        for v in array {
            remove_null_values(v);
        }
    }
}

async fn process_jira_data(
    message: &str,
    jira_data: String,
    chat_completions_url: &str,
    headers: &HeaderMap,
    debug: bool,
) -> Result<Vec<String>, Box<dyn Error>> {
    const BATCH_SIZE: usize = 200_000;
    let jira_data: Value = serde_json::from_str(&jira_data)?;

    if jira_data.as_array().unwrap().len() == 0 {
        return Ok(vec![format!("Context: No Jira data found.")]);
    }

    let mut batches = Vec::new();
    let mut current_batch = Vec::new();
    let mut current_batch_size = 0;

    for issue in jira_data.as_array().unwrap() {
        let issue_str = serde_json::to_string(issue)?;
        if current_batch_size + issue_str.len() > BATCH_SIZE && !current_batch.is_empty() {
            batches.push(current_batch);
            current_batch = Vec::new();
            current_batch_size = 0;
        }
        current_batch.push(issue.clone());
        current_batch_size += issue_str.len();
    }

    if !current_batch.is_empty() {
        batches.push(current_batch);
    }

    if debug {
        println!("DEBUG: Created {} batches of Jira data", batches.len());
    }

    let mut batch_summaries = Vec::new();

    for (i, batch) in batches.into_iter().enumerate() {
        let batch_str = serde_json::to_string(&batch)?;
        let batch_query = format!(
            "Question(s):\n{}\n\nJira data:\n{}\n\nBased on Jira data, please provide a comprehensive answer for the question(s).",
            message,
            batch_str
        );
        let batch_summary = cody_chat(&batch_query, chat_completions_url, headers, debug).await?;
        if debug {
            println!(
                "DEBUG: Processed batch {} answer:\n{}",
                i + 1,
                batch_summary
            );
        }
        batch_summaries.push(batch_summary);
    }

    Ok(batch_summaries)
}

async fn cody_chat(
    query: &str,
    chat_completions_url: &str,
    headers: &HeaderMap,
    debug: bool,
) -> Result<String, Box<dyn Error>> {
    let final_prompt = format!(
        r#"
    You are given the following query:
    {}
    Please provide a concise and informative answer.
    "#,
        query
    );

    let response = chat_completions(&final_prompt, chat_completions_url, headers, debug).await?;
    Ok(response)
}

async fn chat_completions(
    query: &str,
    chat_completions_url: &str,
    headers: &HeaderMap,
    debug: bool,
) -> Result<String, Box<dyn Error>> {
    let data = json!({
        "maxTokensToSample": 4000,
        "messages": [{"speaker": "human", "text": query}],
        "model": "gpt-4o",
        "temperature": 0.2,
        "topK": -1,
        "topP": -1,
        "stream": true,
    });

    let client = Client::builder()
        .timeout(Duration::from_secs(300))
        .build()?;
    let mut response = client
        .post(chat_completions_url)
        .headers(headers.clone())
        .json(&data)
        .send()
        .await?
        .bytes_stream();

    let mut last_response = String::new();
    let mut buffer = String::new();

    while let Some(chunk) = response.next().await {
        let chunk = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(pos) = buffer.find('\n') {
            let line = buffer.drain(..=pos).collect::<String>();
            if line.starts_with("data: ") {
                let data = line.trim_start_matches("data: ");
                if data != "[DONE]" {
                    if let Ok(event_data) = serde_json::from_str::<Value>(data) {
                        if let Some(completion) = event_data["completion"].as_str() {
                            last_response = completion.to_string();
                        }
                    }
                }
            }
        }
    }

    if debug {
        println!("DEBUG: Chat completion response received");
    }

    Ok(last_response)
}