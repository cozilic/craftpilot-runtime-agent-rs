using Xunit;

namespace CraftPilot.AgentSetup.Tests;

public class ComposeConflictTests
{
    [Theory]
    [InlineData(
        "Container craftpilot-runtime-agent-rs  Creating\r\n" +
        "Error response from daemon: Conflict. The container name \"/craftpilot-runtime-agent-rs\" " +
        "is already in use by container \"bf67be6f5e52\". You have to remove (or rename) that container " +
        "to be able to reuse that name.")]
    [InlineData("Error response from daemon: Conflict. The container name is already in use by container \"1234\".")]
    [InlineData("ERROR: the container name is ALREADY IN USE BY CONTAINER \"abc\"")]
    public void a_container_name_conflict_is_recognised(string output)
    {
        Assert.True(ComposeConflict.IsNameConflict(output));
    }

    [Theory]
    [InlineData("")]
    [InlineData("Container craftpilot-runtime-agent-rs  Started")]
    [InlineData("Error response from daemon: No such container: craftpilot-runtime-agent-rs")]
    [InlineData("Error response from daemon: network craftpilot-servers not found")]
    public void other_compose_failures_are_not_conflicts(string output)
    {
        Assert.False(ComposeConflict.IsNameConflict(output));
    }
}
