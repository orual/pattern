//! Voting coordination pattern implementation

use async_trait::async_trait;
use chrono::{Duration, Utc};
use std::{collections::HashMap, sync::Arc};
use uuid::Uuid;

use crate::{
    CoreError, Result,
    agent::Agent,
    coordination::{
        groups::{
            AgentResponse, AgentWithMembership, GroupManager, GroupResponse, GroupResponseEvent,
        },
        types::{
            CoordinationPattern, GroupState, TieBreaker, Vote, VoteOption, VotingProposal,
            VotingRules, VotingSession,
        },
        utils::text_response,
    },
    messages::Message,
};

#[derive(Clone)]
pub struct VotingManager;

#[async_trait]
impl GroupManager for VotingManager {
    async fn route_message(
        &self,
        group: &crate::coordination::groups::AgentGroup,
        agents: &[AgentWithMembership<Arc<dyn Agent>>],
        message: Message,
    ) -> Result<Box<dyn futures::Stream<Item = GroupResponseEvent> + Send + Unpin>> {
        use tokio_stream::wrappers::ReceiverStream;
        let (tx, rx) = tokio::sync::mpsc::channel(100);

        let start_time = std::time::Instant::now();
        let group_id = group.id.clone();
        let _group_name = group.name.clone();

        // Do the full voting operation synchronously first
        let result = self.do_voting(group, agents, message).await;

        // Then send the result as a single Complete event
        tokio::spawn(async move {
            match result {
                Ok((agent_responses, state_changes)) => {
                    let _ = tx
                        .send(GroupResponseEvent::Complete {
                            group_id,
                            pattern: "voting".to_string(),
                            execution_time: start_time.elapsed(),
                            agent_responses,
                            state_changes,
                        })
                        .await;
                }
                Err(e) => {
                    let _ = tx
                        .send(GroupResponseEvent::Error {
                            agent_id: None,
                            message: e.to_string(),
                            recoverable: false,
                        })
                        .await;
                }
            }
        });

        Ok(Box::new(ReceiverStream::new(rx)))
    }

    async fn update_state(
        &self,
        _current_state: &GroupState,
        response: &GroupResponse,
    ) -> Result<Option<GroupState>> {
        // State is already updated in route_message for voting
        Ok(response.state_changes.clone())
    }
}

impl VotingManager {
    async fn do_voting(
        &self,
        group: &crate::coordination::groups::AgentGroup,
        agents: &[AgentWithMembership<Arc<dyn Agent>>],
        message: Message,
    ) -> Result<(Vec<AgentResponse>, Option<GroupState>)> {
        // Extract voting config
        let (quorum, voting_rules) = match &group.coordination_pattern {
            CoordinationPattern::Voting {
                quorum,
                voting_rules,
            } => (*quorum, voting_rules),
            _ => {
                return Err(CoreError::AgentGroupError {
                    group_name: group.name.clone(),
                    operation: "route_message".to_string(),
                    cause: "Invalid pattern for VotingManager".to_string(),
                });
            }
        };

        // Check if we have an active voting session
        let active_session = match &group.state {
            GroupState::Voting { active_session } => active_session.clone(),
            _ => None,
        };

        let mut responses = Vec::new();
        let new_state;

        match active_session {
            None => {
                // Create a new voting session
                let proposal = self.create_proposal_from_message(&message);
                let session = VotingSession {
                    id: Uuid::new_v4(),
                    proposal,
                    votes: HashMap::new(),
                    started_at: Utc::now(),
                    deadline: Utc::now()
                        + Duration::from_std(voting_rules.voting_timeout)
                            .unwrap_or(Duration::seconds(30)),
                };

                // Notify all agents about the new vote
                for awm in agents {
                    if awm.membership.is_active {
                        responses.push(AgentResponse {
                            agent_id: awm.agent.as_ref().id(),
                            response: text_response(format!(
                                "[Voting] New proposal: {}. Options: {:?}",
                                session.proposal.content,
                                session
                                    .proposal
                                    .options
                                    .iter()
                                    .map(|o| &o.description)
                                    .collect::<Vec<_>>()
                            )),
                            responded_at: Utc::now(),
                        });
                    }
                }

                new_state = Some(GroupState::Voting {
                    active_session: Some(session),
                });
            }
            Some(mut session) => {
                // Collect votes (in a real implementation, this would parse agent responses)
                let active_agents: Vec<_> = agents
                    .iter()
                    .filter(|awm| awm.membership.is_active)
                    .collect();

                // Simulate vote collection (in reality, parse from message responses)
                for awm in &active_agents {
                    let agent_id = awm.agent.as_ref().id();
                    if !session.votes.contains_key(&agent_id) {
                        // Simulate a vote
                        if let Some(option) = session.proposal.options.first() {
                            let vote = Vote {
                                option_id: option.id.clone(),
                                weight: 1.0, // Would calculate based on expertise if enabled
                                reasoning: Some("Simulated vote".to_string()),
                                timestamp: Utc::now(),
                            };
                            session.votes.insert(agent_id.clone(), vote);
                        }
                    }
                }

                // Check if we have quorum or timeout
                let has_quorum = session.votes.len() >= quorum;
                let is_timeout = Utc::now() > session.deadline;

                if has_quorum || is_timeout {
                    // Tally votes and determine winner
                    let result = self.tally_votes(&session, voting_rules)?;

                    responses.push(AgentResponse {
                        agent_id: agents[0].agent.as_ref().id(), // Group response
                        response: text_response(format!(
                            "[Voting Complete] Winner: {}. Votes: {}/{}",
                            result,
                            session.votes.len(),
                            active_agents.len()
                        )),
                        responded_at: Utc::now(),
                    });

                    // Clear the voting session
                    new_state = Some(GroupState::Voting {
                        active_session: None,
                    });
                } else {
                    // Still collecting votes
                    responses.push(AgentResponse {
                        agent_id: agents[0].agent.as_ref().id(),
                        response: text_response(format!(
                            "[Voting in Progress] {}/{} votes collected",
                            session.votes.len(),
                            quorum
                        )),
                        responded_at: Utc::now(),
                    });

                    new_state = Some(GroupState::Voting {
                        active_session: Some(session),
                    });
                }
            }
        }

        Ok((responses, new_state))
    }

    fn create_proposal_from_message(&self, message: &Message) -> VotingProposal {
        Self::create_proposal_from_message_impl(message)
    }

    fn create_proposal_from_message_impl(message: &Message) -> VotingProposal {
        // In a real implementation, this would parse the message to create options
        VotingProposal {
            content: format!("Proposal based on: {:?}", message.content),
            options: vec![
                VoteOption {
                    id: "option1".to_string(),
                    description: "Approve".to_string(),
                },
                VoteOption {
                    id: "option2".to_string(),
                    description: "Reject".to_string(),
                },
                VoteOption {
                    id: "option3".to_string(),
                    description: "Abstain".to_string(),
                },
            ],
            metadata: HashMap::new(),
        }
    }

    fn tally_votes(&self, session: &VotingSession, rules: &VotingRules) -> Result<String> {
        Self::tally_votes_impl(session, rules)
    }

    fn tally_votes_impl(session: &VotingSession, rules: &VotingRules) -> Result<String> {
        // Count votes by option
        let mut vote_counts: HashMap<String, f32> = HashMap::new();

        for vote in session.votes.values() {
            *vote_counts.entry(vote.option_id.clone()).or_insert(0.0) += vote.weight;
        }

        // Find the option(s) with the most votes
        let max_votes = vote_counts.values().cloned().fold(0.0, f32::max);
        let winners: Vec<_> = vote_counts
            .iter()
            .filter(|(_, count)| **count == max_votes)
            .map(|(option_id, _)| option_id.clone())
            .collect();

        if winners.len() == 1 {
            // Clear winner
            Ok(winners[0].clone())
        } else {
            // Tie - use tie breaker
            match &rules.tie_breaker {
                TieBreaker::Random => {
                    let mut rng = rand::rng();
                    let index = rand::Rng::random_range(&mut rng, 0..winners.len());
                    winners
                        .get(index)
                        .cloned()
                        .ok_or_else(|| CoreError::AgentGroupError {
                            group_name: "voting".to_string(),
                            operation: "tie_breaker".to_string(),
                            cause: "No winners to choose from".to_string(),
                        })
                }
                TieBreaker::FirstVote => {
                    // Find which tied option got its first vote earliest
                    let mut earliest_vote = None;
                    let mut winning_option = None;

                    for vote in session.votes.values() {
                        if winners.contains(&vote.option_id) {
                            if earliest_vote.is_none() || vote.timestamp < earliest_vote.unwrap() {
                                earliest_vote = Some(vote.timestamp);
                                winning_option = Some(vote.option_id.clone());
                            }
                        }
                    }

                    winning_option.ok_or_else(|| CoreError::AgentGroupError {
                        group_name: "voting".to_string(),
                        operation: "tie_breaker".to_string(),
                        cause: "Could not determine first vote".to_string(),
                    })
                }
                TieBreaker::SpecificAgent(agent_id) => {
                    // Find what the specific agent voted for
                    session
                        .votes
                        .get(agent_id)
                        .map(|vote| vote.option_id.clone())
                        .ok_or_else(|| CoreError::AgentGroupError {
                            group_name: "voting".to_string(),
                            operation: "tie_breaker".to_string(),
                            cause: format!("Tie-breaker agent {} did not vote", agent_id),
                        })
                }
                TieBreaker::NoDecision => Err(CoreError::AgentGroupError {
                    group_name: "voting".to_string(),
                    operation: "tie_breaker".to_string(),
                    cause: "Voting resulted in a tie with no tie-breaker".to_string(),
                }),
            }
        }
    }
}
