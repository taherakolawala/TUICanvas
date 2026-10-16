//! The reviewed set of read operations the reader performs.

use crate::canvas::{CanvasClient, CanvasError, FetchStats};
use crate::model::{self, AssignmentDetail, AssignmentSummary, Course};

pub const PROFILE: &str = "/api/v1/users/self/profile";

pub fn courses_path() -> String {
    "/api/v1/courses?enrollment_state=active&include[]=term&include[]=favorites&per_page=100".into()
}

pub fn assignments_path(course_id: &str) -> String {
    format!(
        "/api/v1/courses/{course_id}/assignments?include[]=submission&order_by=due_at&per_page=100"
    )
}

pub fn assignment_path(course_id: &str, assignment_id: &str) -> String {
    format!("/api/v1/courses/{course_id}/assignments/{assignment_id}?include[]=submission")
}

fn check(id: &str) -> Result<(), CanvasError> {
    if model::valid_id(id) {
        Ok(())
    } else {
        Err(CanvasError::Invalid("invalid identifier"))
    }
}

pub async fn courses(client: &CanvasClient) -> Result<(Vec<Course>, FetchStats), CanvasError> {
    let (mut list, stats): (Vec<Course>, _) = client.get_all_pages(&courses_path(), 10).await?;
    list.retain(Course::visible);
    model::sort_courses(&mut list);
    Ok((list, stats))
}

pub async fn assignments(
    client: &CanvasClient,
    course_id: &str,
) -> Result<(Vec<AssignmentSummary>, FetchStats), CanvasError> {
    check(course_id)?;
    let (mut list, stats): (Vec<AssignmentSummary>, _) = client
        .get_all_pages(&assignments_path(course_id), 20)
        .await?;
    list.retain(|a| model::valid_id(&a.id));
    Ok((list, stats))
}

pub async fn assignment(
    client: &CanvasClient,
    course_id: &str,
    assignment_id: &str,
) -> Result<(AssignmentDetail, FetchStats), CanvasError> {
    check(course_id)?;
    check(assignment_id)?;
    client
        .get_json(&assignment_path(course_id, assignment_id))
        .await
}

#[derive(serde::Deserialize)]
pub struct Profile {
    #[serde(deserialize_with = "de_u64")]
    pub id: u64,
}

fn de_u64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    use serde::Deserialize;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum N {
        S(String),
        N(u64),
    }
    match N::deserialize(d)? {
        N::N(n) => Ok(n),
        N::S(s) => s.parse().map_err(serde::de::Error::custom),
    }
}

pub async fn profile(client: &CanvasClient) -> Result<(Profile, FetchStats), CanvasError> {
    client.get_json(PROFILE).await
}
